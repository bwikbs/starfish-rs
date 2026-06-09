//! E19-M3 `window.matchMedia(query)` — evaluates the media query string ONCE
//! against the render viewport (derived from `DomState.viewport_width`) and
//! returns an inert `MediaQueryList`: `matches`/`media` are read-only data, and
//! every listener method is a no-op (no resize events fire in a one-shot render).

use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsResult, JsString, JsValue, NativeFunction};
use starfish_css::parse_media_query;
use starfish_style::{media_matches, Viewport};

use super::DomState;

/// `window.matchMedia(query)`.
pub(crate) fn match_media(_t: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let query = match args.first() {
        Some(v) => v.to_string(ctx)?.to_std_string_escaped(),
        None => String::new(),
    };
    let vw = ctx
        .realm()
        .host_defined()
        .get::<DomState>()
        .map(|s| s.viewport_width)
        .unwrap_or(800.0);
    let vp = Viewport::from_width(vw);
    let mq = parse_media_query(&query);
    let matches = media_matches(&mq, vp);

    let noop = |name: &str, init: &mut ObjectInitializer<'_>, len: usize| {
        init.function(
            NativeFunction::from_fn_ptr(|_t, _a, _ctx| Ok(JsValue::undefined())),
            js_string!(name),
            len,
        );
    };

    let mut init = ObjectInitializer::new(ctx);
    init.property(
        js_string!("matches"),
        JsValue::from(matches),
        Attribute::READONLY,
    );
    init.property(
        js_string!("media"),
        JsString::from(query),
        Attribute::READONLY,
    );
    init.property(js_string!("onchange"), JsValue::null(), Attribute::all());
    noop("addListener", &mut init, 1);
    noop("removeListener", &mut init, 1);
    noop("addEventListener", &mut init, 2);
    noop("removeEventListener", &mut init, 2);
    noop("dispatchEvent", &mut init, 1);
    Ok(init.build().into())
}
