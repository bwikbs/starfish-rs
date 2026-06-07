//! E8-M1 `getComputedStyle(el)`: an on-demand cascade over the current DOM +
//! the author sheets collected before scripts, returning a read-only object of
//! resolved-value strings for a pragmatic property subset, plus
//! `getPropertyValue(name)`. These are COMPUTED values (cascade + inheritance),
//! NOT post-layout geometry (`offsetWidth`/`getBoundingClientRect` are deferred).

use boa_engine::object::ObjectInitializer;
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsNativeError, JsResult, JsString, JsValue, NativeFunction};
use starfish_style::{Background, ComputedStyle, Display, Length, Rgba, TextAlign};

use super::{DomState, NodeHandle};

// Test-only: counts full style_tree rebuilds; a cache hit does not bump it (E11-M3).
#[cfg(test)]
thread_local! {
    pub(crate) static STYLE_TREE_REBUILDS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// `window.getComputedStyle(el)` — runs `style_tree` over the current DOM and the
/// pre-script author sheets, then returns the queried element's resolved style as
/// a read-only object. A detached / non-element argument yields the initial style.
pub(crate) fn get_computed_style(
    _t: &JsValue,
    args: &[JsValue],
    ctx: &mut Context,
) -> JsResult<JsValue> {
    let el = args
        .first()
        .and_then(|v| v.as_object())
        .and_then(|o| o.downcast_ref::<NodeHandle>().map(|h| h.clone()))
        .ok_or_else(|| {
            JsNativeError::typ().with_message("getComputedStyle: argument is not an element")
        })?;

    let host = ctx.realm().host_defined();
    let state = host
        .get::<DomState>()
        .ok_or_else(|| JsNativeError::typ().with_message("DOM not initialized"))?;
    let cur_ver = state.doc.borrow().mutation_version();
    // Rebuild only when the DOM changed since the cached build (or no cache yet).
    {
        let mut cache = state.styled_cache.borrow_mut();
        let stale = !matches!(&*cache, Some((v, _)) if *v == cur_ver);
        if stale {
            let tree = {
                let doc = state.doc.borrow();
                starfish_style::style_tree(&doc, &state.author_sheets)
            };
            #[cfg(test)]
            STYLE_TREE_REBUILDS.with(|c| c.set(c.get() + 1));
            *cache = Some((cur_ver, tree));
        }
    }
    let computed = {
        let cache = state.styled_cache.borrow();
        let (_, tree) = cache.as_ref().expect("cache filled above");
        tree.get(el.id)
            .cloned()
            .unwrap_or_else(ComputedStyle::initial)
    };
    drop(host); // release the host_defined borrow before taking &mut ctx
    build_computed_style_object(&computed, ctx)
}

/// Resolved value for a kebab-case CSS property name, or `""` for an unlisted one.
fn property_value(c: &ComputedStyle, name: &str) -> String {
    match name {
        "color" => fmt_rgba(c.color),
        "background-color" => match &c.background {
            Background::Color(col) => fmt_rgba(*col),
            Background::Gradient(_) => String::new(),
        },
        "display" => fmt_display(c.display).to_string(),
        "font-size" => format!("{}px", fmt_num(c.font_size)),
        "font-weight" => c.font_weight.0.to_string(),
        "width" => fmt_length(c.width),
        "height" => fmt_length(c.height),
        "margin-top" => fmt_length(c.margin_top),
        "margin-right" => fmt_length(c.margin_right),
        "margin-bottom" => fmt_length(c.margin_bottom),
        "margin-left" => fmt_length(c.margin_left),
        "padding-top" => fmt_length(c.padding_top),
        "padding-right" => fmt_length(c.padding_right),
        "padding-bottom" => fmt_length(c.padding_bottom),
        "padding-left" => fmt_length(c.padding_left),
        "text-align" => fmt_text_align(c.text_align).to_string(),
        "opacity" => fmt_num(c.opacity),
        _ => String::new(),
    }
}

/// camelCase JS property → kebab-case CSS name, for the data-property snapshot.
const PROPS: &[(&str, &str)] = &[
    ("color", "color"),
    ("backgroundColor", "background-color"),
    ("display", "display"),
    ("fontSize", "font-size"),
    ("fontWeight", "font-weight"),
    ("width", "width"),
    ("height", "height"),
    ("marginTop", "margin-top"),
    ("marginRight", "margin-right"),
    ("marginBottom", "margin-bottom"),
    ("marginLeft", "margin-left"),
    ("paddingTop", "padding-top"),
    ("paddingRight", "padding-right"),
    ("paddingBottom", "padding-bottom"),
    ("paddingLeft", "padding-left"),
    ("textAlign", "text-align"),
    ("opacity", "opacity"),
];

/// Build the read-only `CSSStyleDeclaration`-like snapshot object: a read-only
/// data property per listed camelCase name (enumerable) and per kebab name
/// (non-enumerable, for `getPropertyValue` to read off `this`), plus a
/// `getPropertyValue(name)` method.
fn build_computed_style_object(c: &ComputedStyle, ctx: &mut Context) -> JsResult<JsValue> {
    let mut init = ObjectInitializer::new(ctx);
    for &(camel, kebab) in PROPS {
        let v = property_value(c, kebab);
        // Read-only data property: ENUMERABLE only (writable + configurable off).
        init.property(
            js_string!(camel),
            JsString::from(v.clone()),
            Attribute::ENUMERABLE,
        );
        if camel != kebab {
            // Hidden mirror under the kebab name so getPropertyValue resolves it
            // off `this` (no non-Trace Rust capture needed).
            init.property(js_string!(kebab), JsString::from(v), Attribute::empty());
        }
    }
    init.function(
        NativeFunction::from_fn_ptr(get_property_value),
        js_string!("getPropertyValue"),
        1,
    );
    Ok(init.build().into())
}

/// `getPropertyValue(name)` — looks up the (kebab-cased) property mirrored on the
/// snapshot object; unlisted names return `""`.
fn get_property_value(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = match args.first() {
        Some(v) => v
            .to_string(ctx)?
            .to_std_string_escaped()
            .to_ascii_lowercase(),
        None => return Ok(JsString::from("").into()),
    };
    let Some(obj) = this.as_object() else {
        return Ok(JsString::from("").into());
    };
    let key = boa_engine::property::PropertyKey::from(JsString::from(name));
    match obj.get(key, ctx)? {
        v if v.is_string() => Ok(v),
        _ => Ok(JsString::from("").into()),
    }
}

fn fmt_rgba(c: Rgba) -> String {
    if c.a == 255 {
        format!("rgb({}, {}, {})", c.r, c.g, c.b)
    } else {
        format!("rgba({}, {}, {}, {})", c.r, c.g, c.b, (c.a as f32) / 255.0)
    }
}

fn fmt_length(l: Length) -> String {
    match l {
        Length::Px(n) => format!("{}px", fmt_num(n)),
        Length::Percent(n) => format!("{}%", fmt_num(n)),
        Length::Auto => "auto".to_string(),
    }
}

/// Format a number without a trailing `.0` ("16" not "16.0"; "0.5" stays).
fn fmt_num(n: f32) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn fmt_display(d: Display) -> &'static str {
    match d {
        Display::Block => "block",
        Display::Inline => "inline",
        Display::InlineBlock => "inline-block",
        Display::Flex => "flex",
        Display::InlineFlex => "inline-flex",
        Display::Grid => "grid",
        Display::InlineGrid => "inline-grid",
        Display::Table => "table",
        Display::InlineTable => "inline-table",
        Display::TableRowGroup => "table-row-group",
        Display::TableRow => "table-row",
        Display::TableCell => "table-cell",
        Display::None => "none",
    }
}

fn fmt_text_align(a: TextAlign) -> &'static str {
    match a {
        TextAlign::Left => "left",
        TextAlign::Right => "right",
        TextAlign::Center => "center",
        TextAlign::Justify => "justify",
    }
}
