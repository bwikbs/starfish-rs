//! Bounded virtual-time timers: `setTimeout` / `setInterval` / `clearTimeout` /
//! `clearInterval` enqueue into OUR queue (Boa has no host timer). The queue is
//! drained by the run-to-quiescence loop (`lib.rs`) in virtual-time order with
//! no real sleeping; `setInterval` is bounded by a callback-count cap AND a
//! virtual-clock ceiling so it can never hang.

use boa_engine::{Context, Finalize, JsObject, JsResult, JsValue, Trace};

use super::DomState;

/// Total timer callbacks run before the drain gives up.
pub(crate) const MAX_TIMER_CALLBACKS: u32 = 10_000;
/// Virtual-clock ceiling (10 virtual seconds); timers due past it are dropped.
pub(crate) const MAX_VIRTUAL_MS: u64 = 10_000;
/// Virtual frame interval for `requestAnimationFrame` (~60fps).
pub(crate) const RAF_FRAME_MS: u64 = 16;

/// One scheduled callback. `callback` is GC-traced; the scalars are plain data.
#[derive(Trace, Finalize)]
pub(crate) struct Timer {
    #[unsafe_ignore_trace]
    pub id: u64,
    /// Virtual time (ms) the callback is due.
    #[unsafe_ignore_trace]
    pub due_ms: u64,
    /// `Some(period)` for `setInterval`, `None` for one-shot `setTimeout`.
    #[unsafe_ignore_trace]
    pub interval: Option<u64>,
    /// `requestAnimationFrame` callback: receives a virtual timestamp arg.
    #[unsafe_ignore_trace]
    pub is_raf: bool,
    pub callback: JsObject,
}

/// The bounded virtual-time timer queue (lives inside `DomState`).
#[derive(Trace, Finalize, Default)]
pub(crate) struct TimerQueue {
    pub timers: Vec<Timer>,
    #[unsafe_ignore_trace]
    pub next_id: u64,
    /// The virtual clock (ms), starts at 0, advances only to the next due timer.
    #[unsafe_ignore_trace]
    pub now_ms: u64,
    /// Bound counter (§3.4).
    #[unsafe_ignore_trace]
    pub callbacks_run: u32,
}

/// A drained timer step: the callback to invoke (the queue borrow is already
/// dropped, so the caller may freely re-enter JS).
pub(crate) struct TimerStep {
    pub callback: JsObject,
    /// `Some(ts)` for a `requestAnimationFrame` callback (the timestamp arg).
    pub timestamp: Option<f64>,
}

fn callable_arg(args: &[JsValue], i: usize) -> Option<JsObject> {
    args.get(i)
        .and_then(boa_engine::JsValue::as_object)
        .filter(boa_engine::JsObject::is_callable)
}

/// Coerce a `ms`/id argument to a non-negative `u64` (NaN/negative/missing → 0).
fn num_arg(args: &[JsValue], i: usize, ctx: &mut Context) -> u64 {
    match args.get(i) {
        Some(v) => v.to_number(ctx).unwrap_or(0.0).max(0.0) as u64,
        None => 0,
    }
}

fn enqueue(
    ctx: &mut Context,
    cb: JsObject,
    delay_ms: u64,
    interval: Option<u64>,
    is_raf: bool,
) -> u64 {
    let hd = ctx.realm().host_defined();
    let Some(state) = hd.get::<DomState>() else {
        return 0;
    };
    let mut q = state.timers.borrow_mut();
    q.next_id += 1;
    let id = q.next_id;
    let due = q.now_ms.saturating_add(delay_ms);
    q.timers.push(Timer {
        id,
        due_ms: due,
        interval,
        is_raf,
        callback: cb,
    });
    id
}

/// `setTimeout(fn, ms)`. Non-callable first arg (or the `setTimeout("code")`
/// eval-string form) → no-op returning id 0.
pub(crate) fn set_timeout(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let Some(cb) = callable_arg(args, 0) else {
        return Ok(JsValue::from(0));
    };
    let ms = num_arg(args, 1, ctx);
    let id = enqueue(ctx, cb, ms, None, false);
    Ok(JsValue::from(id as f64))
}

/// `setInterval(fn, ms)`. Same as `setTimeout` but re-enqueues each fire.
pub(crate) fn set_interval(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let Some(cb) = callable_arg(args, 0) else {
        return Ok(JsValue::from(0));
    };
    let ms = num_arg(args, 1, ctx);
    let id = enqueue(ctx, cb, ms, Some(ms), false);
    Ok(JsValue::from(id as f64))
}

/// `requestAnimationFrame(fn)`. Enqueues `fn` to run one virtual frame
/// (~16ms) ahead; the drain passes it a virtual timestamp. Returns a handle id.
pub(crate) fn request_animation_frame(
    _t: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let Some(cb) = callable_arg(args, 0) else {
        return Ok(JsValue::from(0));
    };
    let id = enqueue(ctx, cb, RAF_FRAME_MS, None, true);
    Ok(JsValue::from(id as f64))
}

/// `cancelAnimationFrame(id)` — same id space as the other timers.
pub(crate) fn cancel_animation_frame(
    t: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    clear_timer(t, args, ctx)
}

/// `clearTimeout(id)` — also serves `clearInterval` (one id space). Unknown id
/// → no-op.
pub(crate) fn clear_timer(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let id = num_arg(args, 0, ctx);
    if let Some(state) = ctx.realm().host_defined().get::<DomState>() {
        state.timers.borrow_mut().timers.retain(|t| t.id != id);
    }
    Ok(JsValue::undefined())
}

/// Pick the next due timer in virtual-time order (ties: lowest id), advance the
/// virtual clock to it, consume (one-shot) or re-enqueue (interval), and hand
/// back its callback. Returns `None` when the queue is empty or a bound is hit
/// — guaranteeing termination. The queue borrow is dropped before returning so
/// the caller can re-enter JS.
pub(crate) fn next_due_timer(ctx: &mut Context) -> Option<TimerStep> {
    let state = ctx.realm().host_defined();
    let state = state.get::<DomState>()?;
    let mut q = state.timers.borrow_mut();
    if q.callbacks_run >= MAX_TIMER_CALLBACKS {
        return None;
    }
    let idx = q
        .timers
        .iter()
        .enumerate()
        .min_by_key(|(_, t)| (t.due_ms, t.id))?
        .0;
    if q.timers[idx].due_ms > MAX_VIRTUAL_MS {
        return None;
    }
    let cb = q.timers[idx].callback.clone();
    let timestamp = if q.timers[idx].is_raf {
        Some(q.timers[idx].due_ms as f64)
    } else {
        None
    };
    q.now_ms = q.timers[idx].due_ms;
    q.callbacks_run += 1;
    match q.timers[idx].interval {
        // `period.max(1)` floors a zero-delay interval so the virtual clock also
        // creeps up (belt-and-suspenders with the callback-count cap).
        Some(period) => {
            let due = q.now_ms.saturating_add(period.max(1));
            q.timers[idx].due_ms = due;
        }
        None => {
            q.timers.remove(idx);
        }
    }
    Some(TimerStep {
        callback: cb,
        timestamp,
    })
}
