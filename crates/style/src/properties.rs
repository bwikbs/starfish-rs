//! Declaration → typed field application (§5). Reuses M2's typed components.

use starfish_css::{Component, Declaration, Rgba};

use crate::computed::{
    BorderStyle, ComputedStyle, Display, Length, LineHeight, ListStylePosition, ListStyleType,
    TextAlign, TextDecorationLine,
};

const TRANSPARENT: Rgba = Rgba {
    r: 0,
    g: 0,
    b: 0,
    a: 0,
};

/// Per-element resolution context for `em`/`rem`.
#[derive(Clone, Copy)]
pub(crate) struct EmContext {
    /// Parent's computed font-size (basis for `em` on `font-size`).
    pub parent_font_size: f32,
    /// Root element's computed font-size (basis for `rem`).
    pub root_font_size: f32,
}

/// Apply one declaration onto `style`. Returns `true` if it explicitly set the
/// border color (so the cascade can keep currentColor otherwise). Unknown
/// properties / unparseable values are ignored (lenient, never panics).
pub(crate) fn apply_declaration(
    style: &mut ComputedStyle,
    decl: &Declaration,
    ctx: EmContext,
) -> bool {
    let comps = &decl.value.components;
    // `em` on non-font-size lengths resolves against this element's own,
    // already-applied font-size (font-size is applied first; see cascade).
    let em_basis = style.font_size;
    let rem = ctx.root_font_size;

    match decl.name.as_str() {
        "display" => {
            if let Some(d) = as_display(comps) {
                style.display = d;
            }
        }
        "width" => set_len(comps, em_basis, rem, &mut style.width),
        "height" => set_len(comps, em_basis, rem, &mut style.height),

        "margin-top" => set_len(comps, em_basis, rem, &mut style.margin_top),
        "margin-right" => set_len(comps, em_basis, rem, &mut style.margin_right),
        "margin-bottom" => set_len(comps, em_basis, rem, &mut style.margin_bottom),
        "margin-left" => set_len(comps, em_basis, rem, &mut style.margin_left),
        "margin" => {
            if let Some([t, r, b, l]) = shorthand_lengths(comps, em_basis, rem, false) {
                style.margin_top = t;
                style.margin_right = r;
                style.margin_bottom = b;
                style.margin_left = l;
            }
        }

        "padding-top" => set_len(comps, em_basis, rem, &mut style.padding_top),
        "padding-right" => set_len(comps, em_basis, rem, &mut style.padding_right),
        "padding-bottom" => set_len(comps, em_basis, rem, &mut style.padding_bottom),
        "padding-left" => set_len(comps, em_basis, rem, &mut style.padding_left),
        "padding" => {
            if let Some([t, r, b, l]) = shorthand_lengths(comps, em_basis, rem, true) {
                style.padding_top = t;
                style.padding_right = r;
                style.padding_bottom = b;
                style.padding_left = l;
            }
        }

        "border-top-width" => set_px(comps, em_basis, rem, &mut style.border_top_width),
        "border-right-width" => set_px(comps, em_basis, rem, &mut style.border_right_width),
        "border-bottom-width" => set_px(comps, em_basis, rem, &mut style.border_bottom_width),
        "border-left-width" => set_px(comps, em_basis, rem, &mut style.border_left_width),
        "border-width" => {
            if let Some([t, r, b, l]) = shorthand_px(comps, em_basis, rem) {
                style.border_top_width = t;
                style.border_right_width = r;
                style.border_bottom_width = b;
                style.border_left_width = l;
            }
        }
        "border-style" => {
            if let Some(s) = border_style_of(comps) {
                style.border_style = s;
            }
        }
        "border-color" => {
            if let Some(c) = as_color(comps) {
                style.border_color = c;
                return true;
            }
        }
        "border" => return apply_border_shorthand(style, comps, em_basis, rem),

        "color" => {
            if let Some(c) = as_color(comps) {
                style.color = c;
            }
        }
        "background-color" | "background" => {
            if let Some(c) = first_color(comps) {
                style.background_color = c;
            }
        }

        "font-size" => {
            // `em` on font-size resolves against the *parent* font-size.
            if let Some(px) = as_px_with(comps, ctx.parent_font_size, rem) {
                style.font_size = px;
            } else if let Some(pct) = single_percent(comps) {
                style.font_size = ctx.parent_font_size * pct / 100.0;
            }
        }
        "font-weight" => {
            if let Some(w) = font_weight_of(comps) {
                style.font_weight = crate::computed::FontWeight(w);
            }
        }
        "line-height" => {
            if let Some(lh) = line_height_of(comps, em_basis, rem) {
                style.line_height = lh;
            }
        }
        "text-align" => {
            if let Some(a) = text_align_of(comps) {
                style.text_align = a;
            }
        }
        "font-family" => {
            let fam = font_family_of(comps);
            if !fam.is_empty() {
                style.font_family = fam;
            }
        }

        "text-decoration-line" | "text-decoration" => {
            if let Some(line) = text_decoration_of(comps) {
                style.text_decoration_line = line;
            }
        }
        "list-style-type" => {
            if let Some(t) = list_style_type_of(comps) {
                style.list_style_type = t;
            }
        }
        "list-style" => apply_list_style_shorthand(style, comps),
        _ => {}
    }
    false
}

// --- value helpers ---

fn as_length(comps: &[Component], em_basis: f32, rem: f32) -> Option<Length> {
    if comps.len() != 1 {
        return None;
    }
    match &comps[0] {
        Component::Dimension { value, unit } => match unit.as_str() {
            "px" => Some(Length::Px(*value)),
            "%" => Some(Length::Percent(*value)),
            "em" => Some(Length::Px(*value * em_basis)),
            "rem" => Some(Length::Px(*value * rem)),
            _ => None,
        },
        Component::Number(n) if *n == 0.0 => Some(Length::Px(0.0)),
        Component::Keyword(k) if k.eq_ignore_ascii_case("auto") => Some(Length::Auto),
        _ => None,
    }
}

/// Length but `auto` is coerced to `Px(0)` (padding can't be auto).
fn as_length_no_auto(comps: &[Component], em_basis: f32, rem: f32) -> Option<Length> {
    match as_length(comps, em_basis, rem) {
        Some(Length::Auto) => Some(Length::Px(0.0)),
        other => other,
    }
}

fn set_len(comps: &[Component], em_basis: f32, rem: f32, slot: &mut Length) {
    if let Some(l) = as_length(comps, em_basis, rem) {
        *slot = l;
    }
}

/// px-only resolvable forms (border widths / font-size). `em` uses `em_basis`.
fn as_px_with(comps: &[Component], em_basis: f32, rem: f32) -> Option<f32> {
    if comps.len() != 1 {
        return None;
    }
    match &comps[0] {
        Component::Dimension { value, unit } => match unit.as_str() {
            "px" => Some(*value),
            "em" => Some(*value * em_basis),
            "rem" => Some(*value * rem),
            _ => None,
        },
        Component::Number(n) => Some(*n),
        _ => None,
    }
}

fn single_percent(comps: &[Component]) -> Option<f32> {
    match comps {
        [Component::Dimension { value, unit }] if unit == "%" => Some(*value),
        _ => None,
    }
}

fn set_px(comps: &[Component], em_basis: f32, rem: f32, slot: &mut f32) {
    if let Some(px) = as_px_with(comps, em_basis, rem) {
        *slot = px;
    } else if let Some(px) = border_width_keyword(comps) {
        *slot = px;
    }
}

fn border_width_keyword(comps: &[Component]) -> Option<f32> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "thin" => Some(1.0),
            "medium" => Some(3.0),
            "thick" => Some(5.0),
            _ => None,
        },
        _ => None,
    }
}

fn as_color(comps: &[Component]) -> Option<Rgba> {
    match comps {
        [Component::Color(c)] => Some(*c),
        [Component::Keyword(k)] if k.eq_ignore_ascii_case("transparent") => Some(TRANSPARENT),
        _ => None,
    }
}

/// First color in a list (for the `background` shorthand).
fn first_color(comps: &[Component]) -> Option<Rgba> {
    for c in comps {
        match c {
            Component::Color(rgba) => return Some(*rgba),
            Component::Keyword(k) if k.eq_ignore_ascii_case("transparent") => {
                return Some(TRANSPARENT)
            }
            _ => {}
        }
    }
    None
}

fn as_display(comps: &[Component]) -> Option<Display> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "block" => Some(Display::Block),
            "inline" => Some(Display::Inline),
            "inline-block" => Some(Display::InlineBlock),
            "none" => Some(Display::None),
            _ => None,
        },
        _ => None,
    }
}

fn border_style_of(comps: &[Component]) -> Option<BorderStyle> {
    match comps {
        [Component::Keyword(k)] => Some(style_keyword(k)),
        _ => None,
    }
}

fn style_keyword(k: &str) -> BorderStyle {
    match k.to_ascii_lowercase().as_str() {
        "none" | "hidden" => BorderStyle::None,
        _ => BorderStyle::Solid,
    }
}

fn is_style_keyword(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "none" | "hidden" | "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge"
            | "inset" | "outset"
    )
}

fn font_weight_of(comps: &[Component]) -> Option<u16> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "bold" | "bolder" => Some(700),
            "normal" | "lighter" => Some(400),
            _ => None,
        },
        [Component::Number(n)] => Some(*n as u16),
        _ => None,
    }
}

fn line_height_of(comps: &[Component], em_basis: f32, rem: f32) -> Option<LineHeight> {
    match comps {
        [Component::Number(n)] => Some(LineHeight::Number(*n)),
        [Component::Keyword(k)] if k.eq_ignore_ascii_case("normal") => Some(LineHeight::Normal),
        [Component::Dimension { value, unit }] => match unit.as_str() {
            "px" => Some(LineHeight::Px(*value)),
            "em" => Some(LineHeight::Px(*value * em_basis)),
            "rem" => Some(LineHeight::Px(*value * rem)),
            "%" => Some(LineHeight::Number(*value / 100.0)),
            _ => None,
        },
        _ => None,
    }
}

fn text_align_of(comps: &[Component]) -> Option<TextAlign> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "left" => Some(TextAlign::Left),
            "right" => Some(TextAlign::Right),
            "center" => Some(TextAlign::Center),
            "justify" => Some(TextAlign::Justify),
            _ => None,
        },
        _ => None,
    }
}

fn font_family_of(comps: &[Component]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let flush = |cur: &mut Vec<String>, out: &mut Vec<String>| {
        if !cur.is_empty() {
            out.push(cur.join(" "));
            cur.clear();
        }
    };
    for c in comps {
        match c {
            Component::Comma => flush(&mut cur, &mut out),
            Component::Str(s) => cur.push(s.clone()),
            Component::Keyword(k) => cur.push(k.clone()),
            _ => {}
        }
    }
    flush(&mut cur, &mut out);
    out
}

// --- shorthands ---

/// 1–4 lengths → [top, right, bottom, left]. `no_auto` coerces `auto`→`Px(0)`.
/// Each component must parse on its own; otherwise the whole shorthand is None.
fn shorthand_lengths(
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    no_auto: bool,
) -> Option<[Length; 4]> {
    let mut vals = Vec::with_capacity(4);
    for c in comps {
        let slice = std::slice::from_ref(c);
        let l = if no_auto {
            as_length_no_auto(slice, em_basis, rem)
        } else {
            as_length(slice, em_basis, rem)
        };
        vals.push(l?);
    }
    expand4(&vals)
}

fn shorthand_px(comps: &[Component], em_basis: f32, rem: f32) -> Option<[f32; 4]> {
    let mut vals = Vec::with_capacity(4);
    for c in comps {
        let slice = std::slice::from_ref(c);
        let px = as_px_with(slice, em_basis, rem).or_else(|| border_width_keyword(slice))?;
        vals.push(px);
    }
    expand4(&vals)
}

/// CSS 1–4 value expansion to [top, right, bottom, left].
fn expand4<T: Copy>(vals: &[T]) -> Option<[T; 4]> {
    match vals {
        [a] => Some([*a, *a, *a, *a]),
        [v, h] => Some([*v, *h, *v, *h]),
        [t, h, b] => Some([*t, *h, *b, *h]),
        [t, r, b, l] => Some([*t, *r, *b, *l]),
        _ => None,
    }
}

/// `border: <width> || <style> || <color>` in any order → all four widths +
/// shared style + shared color. Returns `true` if a color was set.
fn apply_border_shorthand(
    style: &mut ComputedStyle,
    comps: &[Component],
    em_basis: f32,
    rem: f32,
) -> bool {
    let mut color_set = false;
    for c in comps {
        match c {
            Component::Dimension { .. } | Component::Number(_) => {
                if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem) {
                    style.border_top_width = px;
                    style.border_right_width = px;
                    style.border_bottom_width = px;
                    style.border_left_width = px;
                }
            }
            Component::Color(rgba) => {
                style.border_color = *rgba;
                color_set = true;
            }
            Component::Keyword(k) if k.eq_ignore_ascii_case("transparent") => {
                style.border_color = TRANSPARENT;
                color_set = true;
            }
            Component::Keyword(k) if is_style_keyword(k) => {
                style.border_style = style_keyword(k);
            }
            Component::Keyword(k) => {
                if let Some(px) = border_width_keyword(std::slice::from_ref(c)) {
                    style.border_top_width = px;
                    style.border_right_width = px;
                    style.border_bottom_width = px;
                    style.border_left_width = px;
                } else {
                    let _ = k; // unknown keyword ignored
                }
            }
            _ => {}
        }
    }
    color_set
}

// --- M1: text-decoration / list-style ---

/// Parse `none | [underline || overline || line-through]`. For the
/// `text-decoration` shorthand we ignore any color/style components (M1 always
/// uses the text color + solid). Returns None if no line keyword present.
fn text_decoration_of(comps: &[Component]) -> Option<TextDecorationLine> {
    let mut line = TextDecorationLine::NONE;
    let mut saw_keyword = false;
    for c in comps {
        if let Component::Keyword(k) = c {
            match k.to_ascii_lowercase().as_str() {
                "underline" => {
                    line.insert(TextDecorationLine::UNDERLINE);
                    saw_keyword = true;
                }
                "overline" => {
                    line.insert(TextDecorationLine::OVERLINE);
                    saw_keyword = true;
                }
                "line-through" => {
                    line.insert(TextDecorationLine::LINE_THROUGH);
                    saw_keyword = true;
                }
                "none" => saw_keyword = true, // explicit none → NONE
                _ => {}                       // solid/color/etc. in the shorthand → ignored
            }
        }
    }
    if saw_keyword {
        Some(line)
    } else {
        None
    }
}

fn list_style_type_of(comps: &[Component]) -> Option<ListStyleType> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "disc" => Some(ListStyleType::Disc),
            "circle" => Some(ListStyleType::Circle),
            "square" => Some(ListStyleType::Square),
            "decimal" => Some(ListStyleType::Decimal),
            "none" => Some(ListStyleType::None),
            _ => None,
        },
        _ => None,
    }
}

/// `list-style` shorthand subset: only the `<list-style-type>` keyword is
/// honored; `position`/`image` tokens are ignored (M1). A bare `none` sets the
/// type to `None` (we don't distinguish type-none vs image-none).
fn apply_list_style_shorthand(style: &mut ComputedStyle, comps: &[Component]) {
    for c in comps {
        if let Component::Keyword(k) = c {
            if let Some(t) = list_style_type_of(std::slice::from_ref(c)) {
                style.list_style_type = t;
            } else if k.eq_ignore_ascii_case("outside") || k.eq_ignore_ascii_case("inside") {
                style.list_style_position = ListStylePosition::Outside; // only outside modelled
            }
        }
    }
}
