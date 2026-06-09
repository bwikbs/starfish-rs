//! `element.style` and `element.classList` host objects, built fresh per access
//! over the same [`NodeHandle`]. They read/write the element's `style` / `class`
//! attribute string each call (no live CSSOM / DOMTokenList). Writing `style`
//! is what the cascade's inline-style hook (E4-M2 §6) renders.

use boa_engine::object::{FunctionObjectBuilder, ObjectInitializer};
use boa_engine::property::Attribute;
use boa_engine::{js_string, Context, JsResult, JsString, JsValue, NativeFunction};
use starfish_dom::Document;

use super::NodeHandle;

/// Curated camelCase → kebab-case CSS properties exposed as `style` accessors.
/// Unlisted properties still work via `setProperty`/`cssText`.
const STYLE_PROPS: &[(&str, &str)] = &[
    ("color", "color"),
    ("background", "background"),
    ("backgroundColor", "background-color"),
    ("width", "width"),
    ("height", "height"),
    ("display", "display"),
    ("margin", "margin"),
    ("padding", "padding"),
    ("border", "border"),
    ("fontSize", "font-size"),
    ("fontWeight", "font-weight"),
    ("textAlign", "text-align"),
    ("opacity", "opacity"),
    ("top", "top"),
    ("left", "left"),
];

/// Parse the inline `style` text into `(name, value)` pairs via the css
/// declaration parser (the `*{…}` trick), keeping source order.
fn parse_decls(style: &str) -> Vec<(String, String)> {
    let sheet = starfish_css::parse_stylesheet(&format!("*{{{style}}}"));
    match sheet.rules.into_iter().next() {
        Some(rule) => rule
            .declarations
            .into_iter()
            .map(|d| (d.name, d.value.raw))
            .collect(),
        None => Vec::new(),
    }
}

fn serialize_decls(decls: &[(String, String)]) -> String {
    decls
        .iter()
        .map(|(n, v)| format!("{n}: {v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Read a single property's value out of the element's `style` attribute.
fn get_prop(doc: &Document, h: &NodeHandle, prop: &str) -> String {
    let style = doc.get_attribute(h.id, "style").unwrap_or("");
    parse_decls(style)
        .into_iter()
        .rev() // last declaration wins
        .find(|(n, _)| n == prop)
        .map(|(_, v)| v)
        .unwrap_or_default()
}

/// Upsert (`Some`) or remove (`None`) `prop` in the element's `style` attribute.
fn set_prop(doc: &mut Document, h: &NodeHandle, prop: &str, value: Option<&str>) {
    let style = doc.get_attribute(h.id, "style").unwrap_or("").to_string();
    let mut decls = parse_decls(&style);
    decls.retain(|(n, _)| n != prop);
    if let Some(v) = value {
        if !v.trim().is_empty() {
            decls.push((prop.to_string(), v.trim().to_string()));
        }
    }
    if decls.is_empty() {
        doc.remove_attribute(h.id, "style");
    } else {
        doc.set_attribute(h.id, "style", &serialize_decls(&decls));
    }
}

fn arg_str(args: &[JsValue], i: usize, ctx: &mut Context) -> JsResult<String> {
    match args.get(i) {
        Some(v) => Ok(v.to_string(ctx)?.to_std_string_escaped()),
        None => Ok(String::new()),
    }
}

/// Build the `el.style` object: `setProperty`/`getPropertyValue`/
/// `removeProperty`, a `cssText` accessor, and the curated camelCase accessors.
pub(crate) fn style_object(h: NodeHandle, ctx: &mut Context) -> JsResult<JsValue> {
    let mut init = ObjectInitializer::new(ctx);

    init.function(
        {
            let h = h.clone();
            NativeFunction::from_copy_closure_with_captures(
                move |_t, args, cap: &NodeHandle, ctx| {
                    let name = arg_str(args, 0, ctx)?.to_ascii_lowercase();
                    let value = arg_str(args, 1, ctx)?;
                    set_prop(&mut cap.shared.borrow_mut(), cap, &name, Some(&value));
                    super::observer::record_attribute(ctx, cap.id, "style");
                    Ok(JsValue::undefined())
                },
                h,
            )
        },
        js_string!("setProperty"),
        2,
    );
    init.function(
        {
            let h = h.clone();
            NativeFunction::from_copy_closure_with_captures(
                move |_t, args, cap: &NodeHandle, ctx| {
                    let name = arg_str(args, 0, ctx)?.to_ascii_lowercase();
                    let v = get_prop(&cap.shared.borrow(), cap, &name);
                    Ok(JsString::from(v).into())
                },
                h,
            )
        },
        js_string!("getPropertyValue"),
        1,
    );
    init.function(
        {
            let h = h.clone();
            NativeFunction::from_copy_closure_with_captures(
                move |_t, args, cap: &NodeHandle, ctx| {
                    let name = arg_str(args, 0, ctx)?.to_ascii_lowercase();
                    set_prop(&mut cap.shared.borrow_mut(), cap, &name, None);
                    super::observer::record_attribute(ctx, cap.id, "style");
                    Ok(JsValue::undefined())
                },
                h,
            )
        },
        js_string!("removeProperty"),
        1,
    );

    // cssText accessor: get/set the whole `style` attribute.
    accessor_prop(
        &mut init,
        "cssText",
        h.clone(),
        |doc, h| doc.get_attribute(h.id, "style").unwrap_or("").to_string(),
        |doc, h, v| {
            if v.trim().is_empty() {
                doc.remove_attribute(h.id, "style");
            } else {
                doc.set_attribute(h.id, "style", &v);
            }
        },
    );

    // Curated camelCase property accessors.
    for &(camel, kebab) in STYLE_PROPS {
        accessor_prop(
            &mut init,
            camel,
            h.clone(),
            move |doc, h| get_prop(doc, h, kebab),
            move |doc, h, v| set_prop(doc, h, kebab, Some(&v)),
        );
    }

    Ok(init.build().into())
}

/// Add a `get`/`set` accessor to a plain `style`/`classList` object whose
/// getter/setter close over a `NodeHandle` + the given arena read/write fns.
fn accessor_prop(
    init: &mut ObjectInitializer<'_>,
    name: &str,
    handle: NodeHandle,
    get: impl Fn(&Document, &NodeHandle) -> String + Copy + 'static,
    set: impl Fn(&mut Document, &NodeHandle, String) + Copy + 'static,
) {
    let realm = init.context().realm().clone();
    let getter = {
        let h = handle.clone();
        NativeFunction::from_copy_closure_with_captures(
            move |_t, _a, cap: &NodeHandle, _ctx| {
                let v = get(&cap.shared.borrow(), cap);
                Ok(JsString::from(v).into())
            },
            h,
        )
    };
    let setter = NativeFunction::from_copy_closure_with_captures(
        move |_t, args, cap: &NodeHandle, ctx| {
            let v = arg_str(args, 0, ctx)?;
            set(&mut cap.shared.borrow_mut(), cap, v);
            super::observer::record_attribute(ctx, cap.id, "style");
            Ok(JsValue::undefined())
        },
        handle,
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

// --- classList ---

fn class_set(doc: &Document, h: &NodeHandle) -> Vec<String> {
    doc.get_attribute(h.id, "class")
        .unwrap_or("")
        .split_ascii_whitespace()
        .map(|s| s.to_string())
        .collect()
}

fn write_class_set(doc: &mut Document, h: &NodeHandle, set: &[String]) {
    if set.is_empty() {
        doc.remove_attribute(h.id, "class");
    } else {
        doc.set_attribute(h.id, "class", &set.join(" "));
    }
}

/// Build the `el.classList` object: `add`/`remove`/`contains`/`toggle` over the
/// `class` attribute as a space-split set.
pub(crate) fn class_list_object(h: NodeHandle, ctx: &mut Context) -> JsResult<JsValue> {
    let mut init = ObjectInitializer::new(ctx);

    init.function(
        {
            let h = h.clone();
            NativeFunction::from_copy_closure_with_captures(
                move |_t, args, cap: &NodeHandle, ctx| {
                    let c = arg_str(args, 0, ctx)?;
                    {
                        let mut doc = cap.shared.borrow_mut();
                        let mut set = class_set(&doc, cap);
                        if !c.is_empty() && !set.contains(&c) {
                            set.push(c);
                            write_class_set(&mut doc, cap, &set);
                        }
                    }
                    super::observer::record_attribute(ctx, cap.id, "class");
                    Ok(JsValue::undefined())
                },
                h,
            )
        },
        js_string!("add"),
        1,
    );
    init.function(
        {
            let h = h.clone();
            NativeFunction::from_copy_closure_with_captures(
                move |_t, args, cap: &NodeHandle, ctx| {
                    let c = arg_str(args, 0, ctx)?;
                    {
                        let mut doc = cap.shared.borrow_mut();
                        let mut set = class_set(&doc, cap);
                        set.retain(|x| *x != c);
                        write_class_set(&mut doc, cap, &set);
                    }
                    super::observer::record_attribute(ctx, cap.id, "class");
                    Ok(JsValue::undefined())
                },
                h,
            )
        },
        js_string!("remove"),
        1,
    );
    init.function(
        {
            let h = h.clone();
            NativeFunction::from_copy_closure_with_captures(
                move |_t, args, cap: &NodeHandle, ctx| {
                    let c = arg_str(args, 0, ctx)?;
                    let doc = cap.shared.borrow();
                    Ok(JsValue::from(class_set(&doc, cap).contains(&c)))
                },
                h,
            )
        },
        js_string!("contains"),
        1,
    );
    init.function(
        {
            let h = h.clone();
            NativeFunction::from_copy_closure_with_captures(
                move |_t, args, cap: &NodeHandle, ctx| {
                    let c = arg_str(args, 0, ctx)?;
                    // optional force argument
                    let force = args.get(1).map(|v| v.to_boolean());
                    let add = {
                        let mut doc = cap.shared.borrow_mut();
                        let mut set = class_set(&doc, cap);
                        let present = set.contains(&c);
                        let add = force.unwrap_or(!present);
                        if add && !present {
                            set.push(c);
                        } else if !add && present {
                            set.retain(|x| *x != c);
                        }
                        write_class_set(&mut doc, cap, &set);
                        add
                    };
                    super::observer::record_attribute(ctx, cap.id, "class");
                    Ok(JsValue::from(add))
                },
                h,
            )
        },
        js_string!("toggle"),
        1,
    );
    init.function(
        {
            let h = h.clone();
            NativeFunction::from_copy_closure_with_captures(
                move |_t, args, cap: &NodeHandle, ctx| {
                    let old = arg_str(args, 0, ctx)?;
                    let new = arg_str(args, 1, ctx)?;
                    {
                        let mut doc = cap.shared.borrow_mut();
                        let mut set = class_set(&doc, cap);
                        if !set.contains(&old) {
                            return Ok(JsValue::from(false));
                        }
                        // Replace old's token in place (first position), drop any
                        // other occurrence of new to keep the set unique.
                        let mut out: Vec<String> = Vec::with_capacity(set.len());
                        for tok in set.drain(..) {
                            if tok == old {
                                out.push(new.clone());
                            } else if tok != new {
                                out.push(tok);
                            }
                        }
                        write_class_set(&mut doc, cap, &out);
                    }
                    super::observer::record_attribute(ctx, cap.id, "class");
                    Ok(JsValue::from(true))
                },
                h,
            )
        },
        js_string!("replace"),
        2,
    );
    init.function(
        {
            let h = h.clone();
            NativeFunction::from_copy_closure_with_captures(
                move |_t, args, cap: &NodeHandle, ctx| {
                    let idx = args.first().map(|v| v.to_number(ctx)).transpose()?;
                    let doc = cap.shared.borrow();
                    let set = class_set(&doc, cap);
                    let i = idx.unwrap_or(0.0);
                    if i < 0.0 || i.fract() != 0.0 {
                        return Ok(JsValue::null());
                    }
                    match set.get(i as usize) {
                        Some(s) => Ok(JsString::from(s.as_str()).into()),
                        None => Ok(JsValue::null()),
                    }
                },
                h,
            )
        },
        js_string!("item"),
        1,
    );

    // `length` accessor: token count. accessor_prop is String-only, so build
    // the numeric getter inline (mirrors accessor_prop's structure).
    let realm = init.context().realm().clone();
    let getter = {
        let h = h.clone();
        NativeFunction::from_copy_closure_with_captures(
            move |_t, _a, cap: &NodeHandle, _ctx| {
                let doc = cap.shared.borrow();
                Ok(JsValue::from(class_set(&doc, cap).len() as u32))
            },
            h,
        )
    };
    let getter = FunctionObjectBuilder::new(&realm, getter).build();
    init.accessor(js_string!("length"), Some(getter), None, Attribute::all());

    Ok(init.build().into())
}
