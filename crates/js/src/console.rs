//! Capturing `console` object. Each of `log/info/warn/error/debug` is a native
//! closure that stringifies its arguments (space-joined) and pushes a
//! `ConsoleMessage` into a shared `Rc<RefCell<Vec<ConsoleMessage>>>` returned to
//! `run_scripts`. We capture rather than print (unlike `boa_runtime::Console`).

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{
    js_string, object::ObjectInitializer, property::Attribute, Context, JsValue, NativeFunction,
};
use boa_gc::{Finalize, Trace};

use crate::{ConsoleLevel, ConsoleMessage};

/// The shared sink the `console` closures push into. A Trace-able newtype so it
/// can be captured by `NativeFunction::from_copy_closure_with_captures`. It holds
/// no `Gc` pointers, so the trace is empty.
#[derive(Clone, Finalize)]
struct ConsoleSink(Rc<RefCell<Vec<ConsoleMessage>>>);

// SAFETY: `ConsoleSink` contains only an `Rc<RefCell<Vec<ConsoleMessage>>>` —
// no Boa `Gc` pointers — so there is nothing for the collector to trace.
unsafe impl Trace for ConsoleSink {
    boa_gc::empty_trace!();
}

/// Install a `console` object on the global and return the shared sink the
/// closures push captured messages into.
pub(crate) fn install(ctx: &mut Context) -> Rc<RefCell<Vec<ConsoleMessage>>> {
    let sink = Rc::new(RefCell::new(Vec::new()));
    let console = ObjectInitializer::new(ctx)
        .function(make_logger(ConsoleLevel::Log, &sink), js_string!("log"), 0)
        .function(make_logger(ConsoleLevel::Info, &sink), js_string!("info"), 0)
        .function(make_logger(ConsoleLevel::Warn, &sink), js_string!("warn"), 0)
        .function(make_logger(ConsoleLevel::Error, &sink), js_string!("error"), 0)
        .function(make_logger(ConsoleLevel::Debug, &sink), js_string!("debug"), 0)
        // E8-M3: `dir`/`table` alias `log` (no inspector / ASCII grid — log the
        // coerced value); `assert` logs an Error-level line when the first arg is
        // falsy.
        .function(make_logger(ConsoleLevel::Log, &sink), js_string!("dir"), 0)
        .function(make_logger(ConsoleLevel::Log, &sink), js_string!("table"), 0)
        .function(make_assert(&sink), js_string!("assert"), 0)
        .build();
    // register_global_property can only fail for an existing non-configurable
    // property; `console` is fresh, so this never errors in practice.
    let _ = ctx.register_global_property(js_string!("console"), console, Attribute::all());
    sink
}

/// Build one native console method at `level` capturing a clone of `sink`.
fn make_logger(level: ConsoleLevel, sink: &Rc<RefCell<Vec<ConsoleMessage>>>) -> NativeFunction {
    let captured = ConsoleSink(sink.clone());
    NativeFunction::from_copy_closure_with_captures(
        move |_this, args, captures: &ConsoleSink, ctx| {
            let text = stringify_args(args, ctx);
            captures.0.borrow_mut().push(ConsoleMessage { level, text });
            Ok(JsValue::undefined())
        },
        captured,
    )
}

/// E8-M3: `console.assert(cond, ...msg)` — when `cond` is falsy, push an
/// `Error`-level "Assertion failed[: msg]" line; when truthy, do nothing. Unlike
/// `make_logger`, this inspects `args[0]` and slices `args[1..]`, so it needs its
/// own closure (over the same sink).
fn make_assert(sink: &Rc<RefCell<Vec<ConsoleMessage>>>) -> NativeFunction {
    let captured = ConsoleSink(sink.clone());
    NativeFunction::from_copy_closure_with_captures(
        move |_this, args, captures: &ConsoleSink, ctx| {
            let cond = args.first().map(|v| v.to_boolean()).unwrap_or(false);
            if !cond {
                let rest = stringify_args(args.get(1..).unwrap_or(&[]), ctx);
                let text = if rest.is_empty() {
                    "Assertion failed".to_string()
                } else {
                    format!("Assertion failed: {rest}")
                };
                captures.0.borrow_mut().push(ConsoleMessage {
                    level: ConsoleLevel::Error,
                    text,
                });
            }
            Ok(JsValue::undefined())
        },
        captured,
    )
}

/// Stringify each argument via primitive `to_string` coercion and join with a
/// single space (M1's "capture what scripts log"). A value that fails to coerce
/// (a thrown `toString`) renders as `<unprintable>` rather than aborting.
fn stringify_args(args: &[JsValue], ctx: &mut Context) -> String {
    args.iter()
        .map(|a| match a.to_string(ctx) {
            Ok(s) => s.to_std_string_escaped(),
            Err(_) => "<unprintable>".to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}
