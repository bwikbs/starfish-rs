//! Declaration → typed field application (§5). Reuses M2's typed components.

use starfish_css::{Component, Declaration, Rgba};

use crate::computed::{
    AlignItems, AlignSelf, Background, BorderStyle, BoxShadow, Clear, ComputedStyle, Display,
    FlexDirection, FlexWrap, Float, GradientStop, JustifyContent, Length, LineHeight, LinearGradient,
    ListStylePosition, ListStyleType, Position, TextAlign, TextDecorationLine,
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
        "background-color" | "background" | "background-image" => {
            if let Some(bg) = parse_background(comps) {
                style.background = bg;
            }
        }
        "border-radius" => {
            if let Some(r) = border_radius_shorthand(comps, em_basis, rem) {
                style.border_radius = r;
            }
        }
        "box-shadow" => {
            if let Some(s) = parse_box_shadow(comps, em_basis, rem) {
                style.box_shadow = Some(s);
            }
        }
        "opacity" => {
            if let Some(n) = single_number(comps) {
                style.opacity = n.clamp(0.0, 1.0);
            } else if let Some(p) = single_percent(comps) {
                style.opacity = (p / 100.0).clamp(0.0, 1.0);
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

        "position" => {
            if let Some(p) = position_of(comps) {
                style.position = p;
            }
        }
        "float" => {
            if let Some(f) = float_of(comps) {
                style.float = f;
            }
        }
        "clear" => {
            if let Some(c) = clear_of(comps) {
                style.clear = c;
            }
        }
        "top" => set_len(comps, em_basis, rem, &mut style.top),
        "right" => set_len(comps, em_basis, rem, &mut style.right),
        "bottom" => set_len(comps, em_basis, rem, &mut style.bottom),
        "left" => set_len(comps, em_basis, rem, &mut style.left),

        "flex-direction" => {
            if let Some(d) = flex_direction_of(comps) {
                style.flex_direction = d;
            }
        }
        "flex-wrap" => {
            if let Some(w) = flex_wrap_of(comps) {
                style.flex_wrap = w;
            }
        }
        "justify-content" => {
            if let Some(j) = justify_content_of(comps) {
                style.justify_content = j;
            }
        }
        "align-items" => {
            if let Some(a) = align_items_of(comps) {
                style.align_items = a;
            }
        }
        "align-self" => {
            if let Some(a) = align_self_of(comps) {
                style.align_self = a;
            }
        }
        "flex-grow" => {
            if let Some(g) = single_number(comps) {
                style.flex_grow = g.max(0.0);
            }
        }
        "flex-shrink" => {
            if let Some(s) = single_number(comps) {
                style.flex_shrink = s.max(0.0);
            }
        }
        "flex-basis" => set_len(comps, em_basis, rem, &mut style.flex_basis),
        "flex" => apply_flex_shorthand(style, comps, em_basis, rem),

        "row-gap" => set_len_no_auto(comps, em_basis, rem, &mut style.row_gap),
        "column-gap" => set_len_no_auto(comps, em_basis, rem, &mut style.column_gap),
        "gap" => apply_gap_shorthand(style, comps, em_basis, rem),
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

// --- E2-M5: background / linear-gradient / border-radius / box-shadow ---

/// `background` value → a `Background`. A `linear-gradient(...)` function wins;
/// else the first color (existing behaviour); else leave unchanged (None).
fn parse_background(comps: &[Component]) -> Option<Background> {
    for c in comps {
        if let Component::Function { name, raw_args } = c {
            if name.eq_ignore_ascii_case("linear-gradient") {
                return parse_linear_gradient(raw_args).map(Background::Gradient);
            }
        }
    }
    first_color(comps).map(Background::Color)
}

/// Parse the verbatim inner args of `linear-gradient(...)`. Splits on top-level
/// commas; the first segment is an optional `<angle>`/`to <side>`, the rest are
/// color stops. Needs ≥ 2 valid stops. (E2-M5 §1.3)
fn parse_linear_gradient(raw_args: &str) -> Option<LinearGradient> {
    let segs = split_top_level_commas(raw_args);
    let mut iter = segs.iter().peekable();
    let mut angle_deg = 180.0; // default: to bottom

    if let Some(first) = iter.peek() {
        if let Some(a) = parse_angle_or_side(first) {
            angle_deg = a;
            iter.next();
        }
    }
    let mut stops = Vec::new();
    for seg in iter {
        if let Some(s) = parse_color_stop(seg) {
            stops.push(s);
        }
    }
    if stops.len() < 2 {
        return None;
    }
    Some(LinearGradient { angle_deg, stops })
}

/// Split a string on commas that are not nested inside parentheses (so an
/// `rgba(…)` stays whole). Each segment is trimmed; empty segments are dropped.
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth = (depth - 1).max(0);
                cur.push(ch);
            }
            ',' if depth == 0 => {
                let t = cur.trim();
                if !t.is_empty() {
                    out.push(t.to_string());
                }
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    let t = cur.trim();
    if !t.is_empty() {
        out.push(t.to_string());
    }
    out
}

/// `"<n>deg"` → `n`; `"to <side(s)>"` → the fixed CSS angle. `None` if the
/// segment is actually a color stop (so the caller treats it as the first stop).
/// `rad`/`turn`/`grad` fold to 0 (M5 deviation). (E2-M5 §1.2)
fn parse_angle_or_side(seg: &str) -> Option<f32> {
    let lower = seg.trim().to_ascii_lowercase();
    if let Some(rest) = lower.strip_suffix("deg") {
        return rest.trim().parse::<f32>().ok();
    }
    if lower.ends_with("rad") || lower.ends_with("turn") || lower.ends_with("grad") {
        // unsupported angle unit → fold to 0deg (to top), per §6.
        let head = lower
            .trim_end_matches("grad")
            .trim_end_matches("turn")
            .trim_end_matches("rad");
        if head.trim().parse::<f32>().is_ok() {
            return Some(0.0);
        }
    }
    let mut words = lower.split_ascii_whitespace();
    if words.next() != Some("to") {
        return None;
    }
    let sides: Vec<&str> = words.collect();
    let has = |w: &str| sides.contains(&w);
    let (top, bottom, left, right) = (has("top"), has("bottom"), has("left"), has("right"));
    match (top, right, bottom, left) {
        (true, false, false, false) => Some(0.0),
        (false, true, false, false) => Some(90.0),
        (false, false, true, false) => Some(180.0),
        (false, false, false, true) => Some(270.0),
        (true, true, false, false) => Some(45.0),
        (false, true, true, false) => Some(135.0),
        (false, false, true, true) => Some(225.0),
        (true, false, false, true) => Some(315.0),
        _ => None,
    }
}

/// `<color> <position>?` → a stop. `%` → fraction; `px`/missing → `None`
/// (px positions ignored in M5, §6).
fn parse_color_stop(seg: &str) -> Option<GradientStop> {
    let mut parts = seg.split_ascii_whitespace();
    let color = starfish_css::parse_color(parts.next()?)?;
    let pos = parts.next().and_then(|p| {
        p.strip_suffix('%')
            .and_then(|n| n.trim().parse::<f32>().ok())
            .map(|n| n / 100.0)
    });
    Some(GradientStop { color, pos })
}

/// 1–4 px values → `[TL, TR, BR, BL]` via CSS corner expansion. Stops at the
/// first `/` (elliptical vertical radii ignored, §6). `%`/non-px → ignored.
fn border_radius_shorthand(comps: &[Component], em_basis: f32, rem: f32) -> Option<[f32; 4]> {
    let mut vals = Vec::with_capacity(4);
    for c in comps {
        match c {
            // a `/` arrives as a raw token; stop reading the horizontal radii.
            Component::Raw(t) if t == "/" => break,
            _ => {
                if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem) {
                    vals.push(px.max(0.0));
                }
            }
        }
    }
    match vals.as_slice() {
        [a] => Some([*a, *a, *a, *a]),
        [a, b] => Some([*a, *b, *a, *b]),
        [a, b, c] => Some([*a, *b, *c, *b]),
        [a, b, c, d] => Some([*a, *b, *c, *d]),
        _ => None,
    }
}

/// `<offset-x> <offset-y> <blur>? <spread>? <color>`, outset only, single
/// shadow. `none` → None. `inset` ignored (treated as outset, §6).
fn parse_box_shadow(comps: &[Component], em_basis: f32, rem: f32) -> Option<BoxShadow> {
    let mut lengths = Vec::new();
    let mut color = Rgba { r: 0, g: 0, b: 0, a: 255 }; // default ≈ currentColor → black
    for c in comps {
        match c {
            Component::Dimension { .. } | Component::Number(_) => {
                if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem) {
                    lengths.push(px);
                }
            }
            Component::Color(rgba) => color = *rgba,
            Component::Keyword(k) if k.eq_ignore_ascii_case("none") => return None,
            _ => {}
        }
    }
    match lengths.as_slice() {
        [x, y] => Some(BoxShadow { offset_x: *x, offset_y: *y, blur: 0.0, spread: 0.0, color }),
        [x, y, b] => Some(BoxShadow { offset_x: *x, offset_y: *y, blur: *b, spread: 0.0, color }),
        [x, y, b, s] => Some(BoxShadow { offset_x: *x, offset_y: *y, blur: *b, spread: *s, color }),
        _ => None,
    }
}

fn as_display(comps: &[Component]) -> Option<Display> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "block" => Some(Display::Block),
            "inline" => Some(Display::Inline),
            "inline-block" => Some(Display::InlineBlock),
            "flex" => Some(Display::Flex),
            "inline-flex" => Some(Display::InlineFlex),
            "none" => Some(Display::None),
            _ => None,
        },
        _ => None,
    }
}

fn flex_direction_of(comps: &[Component]) -> Option<FlexDirection> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "row" => Some(FlexDirection::Row),
            "row-reverse" => Some(FlexDirection::RowReverse),
            "column" => Some(FlexDirection::Column),
            "column-reverse" => Some(FlexDirection::ColumnReverse),
            _ => None,
        },
        _ => None,
    }
}

fn flex_wrap_of(comps: &[Component]) -> Option<FlexWrap> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "nowrap" => Some(FlexWrap::Nowrap),
            "wrap" => Some(FlexWrap::Wrap),
            // `wrap-reverse` deferred → None (ignored).
            _ => None,
        },
        _ => None,
    }
}

fn justify_content_of(comps: &[Component]) -> Option<JustifyContent> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "flex-start" | "start" | "left" => Some(JustifyContent::FlexStart),
            "flex-end" | "end" | "right" => Some(JustifyContent::FlexEnd),
            "center" => Some(JustifyContent::Center),
            "space-between" => Some(JustifyContent::SpaceBetween),
            "space-around" => Some(JustifyContent::SpaceAround),
            "space-evenly" => Some(JustifyContent::SpaceEvenly),
            _ => None,
        },
        _ => None,
    }
}

fn align_items_of(comps: &[Component]) -> Option<AlignItems> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "stretch" => Some(AlignItems::Stretch),
            "flex-start" | "start" => Some(AlignItems::FlexStart),
            "flex-end" | "end" => Some(AlignItems::FlexEnd),
            "center" => Some(AlignItems::Center),
            "baseline" => Some(AlignItems::Baseline),
            _ => None,
        },
        _ => None,
    }
}

fn align_self_of(comps: &[Component]) -> Option<AlignSelf> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "auto" => Some(AlignSelf::Auto),
            "stretch" => Some(AlignSelf::Stretch),
            "flex-start" | "start" => Some(AlignSelf::FlexStart),
            "flex-end" | "end" => Some(AlignSelf::FlexEnd),
            "center" => Some(AlignSelf::Center),
            "baseline" => Some(AlignSelf::Baseline),
            _ => None,
        },
        _ => None,
    }
}

/// A single bare number (used by `flex-grow`/`flex-shrink`).
fn single_number(comps: &[Component]) -> Option<f32> {
    match comps {
        [Component::Number(n)] => Some(*n),
        _ => None,
    }
}

fn set_len_no_auto(comps: &[Component], em_basis: f32, rem: f32, slot: &mut Length) {
    if let Some(l) = as_length_no_auto(comps, em_basis, rem) {
        *slot = l;
    }
}

/// `flex` shorthand. Sets flex_grow / flex_shrink / flex_basis together.
fn apply_flex_shorthand(style: &mut ComputedStyle, comps: &[Component], em_basis: f32, rem: f32) {
    // keyword forms
    if let [Component::Keyword(k)] = comps {
        match k.to_ascii_lowercase().as_str() {
            "none" => {
                style.flex_grow = 0.0;
                style.flex_shrink = 0.0;
                style.flex_basis = Length::Auto;
                return;
            }
            "auto" => {
                style.flex_grow = 1.0;
                style.flex_shrink = 1.0;
                style.flex_basis = Length::Auto;
                return;
            }
            "initial" => {
                style.flex_grow = 0.0;
                style.flex_shrink = 1.0;
                style.flex_basis = Length::Auto;
                return;
            }
            _ => {}
        }
    }
    // numeric / length form. A bare `Number` is always a grow/shrink value,
    // never a basis (so `flex: 1` = grow 1, not basis 1px). Only a `Dimension`
    // or the `auto` keyword is treated as the basis.
    let mut grow: Option<f32> = None;
    let mut shrink: Option<f32> = None;
    let mut basis: Option<Length> = None;
    for c in comps {
        match c {
            Component::Number(n) => {
                if grow.is_none() {
                    grow = Some(*n);
                } else if shrink.is_none() {
                    shrink = Some(*n);
                }
            }
            Component::Dimension { .. } => {
                if let Some(l) = as_length(std::slice::from_ref(c), em_basis, rem) {
                    basis = Some(l);
                }
            }
            Component::Keyword(k) if k.eq_ignore_ascii_case("auto") => {
                basis = Some(Length::Auto);
            }
            _ => {}
        }
    }
    if grow.is_some() || shrink.is_some() || basis.is_some() {
        style.flex_grow = grow.unwrap_or(0.0).max(0.0);
        style.flex_shrink = shrink.unwrap_or(1.0).max(0.0);
        style.flex_basis = basis.unwrap_or(Length::Px(0.0)); // omitted basis = 0
    }
}

/// `gap` shorthand (`gap: <row> [<column>]`; one value sets both).
fn apply_gap_shorthand(style: &mut ComputedStyle, comps: &[Component], em_basis: f32, rem: f32) {
    let mut lens = Vec::with_capacity(2);
    for c in comps {
        if let Some(l) = as_length_no_auto(std::slice::from_ref(c), em_basis, rem) {
            lens.push(l);
        }
    }
    match lens.as_slice() {
        [g] => {
            style.row_gap = *g;
            style.column_gap = *g;
        }
        [r, c, ..] => {
            style.row_gap = *r;
            style.column_gap = *c;
        }
        [] => {}
    }
}

fn position_of(comps: &[Component]) -> Option<Position> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "static" => Some(Position::Static),
            "relative" => Some(Position::Relative),
            "absolute" => Some(Position::Absolute),
            "fixed" => Some(Position::Fixed),
            _ => None,
        },
        _ => None,
    }
}

fn float_of(comps: &[Component]) -> Option<Float> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "none" => Some(Float::None),
            "left" => Some(Float::Left),
            "right" => Some(Float::Right),
            _ => None,
        },
        _ => None,
    }
}

fn clear_of(comps: &[Component]) -> Option<Clear> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "none" => Some(Clear::None),
            "left" => Some(Clear::Left),
            "right" => Some(Clear::Right),
            "both" => Some(Clear::Both),
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
