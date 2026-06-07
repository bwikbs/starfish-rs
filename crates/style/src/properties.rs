//! Declaration → typed field application (§5). Reuses M2's typed components.

use starfish_css::{Component, Declaration, Rgba};
use starfish_dom::{Document, NodeId};

use crate::computed::{
    AlignItems, AlignSelf, Background, BorderCollapse, BorderStyle, BoxShadow, Clear, ComputedStyle,
    Content,
    Direction, Display, FlexDirection, FlexWrap, Float, FontStyle, GradientStop, GridLine,
    GridPlacement, JustifyContent, Length, LengthPct, LineHeight, LinearGradient, ListStylePosition,
    ListStyleType, Position, TextAlign, TextDecorationLine, TextTransform, TrackSize, TransformFn,
    UnicodeBidi, WhiteSpace,
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

/// Resolve a `content` declaration's value to a [`Content`], given the
/// originating element so `attr()` can look up an attribute (E7-M2). Grammar:
/// `none`/`normal` → no box; `<string>+` / `attr(name)` / their concatenation →
/// `Text`; any unsupported component (counter/url/quote/…) → `None` (no box).
pub(crate) fn resolve_content(doc: &Document, element: NodeId, decl: &Declaration) -> Content {
    let comps = &decl.value.components;
    if comps.len() == 1 {
        if let Component::Keyword(k) = &comps[0] {
            if k.eq_ignore_ascii_case("none") {
                return Content::None;
            }
            if k.eq_ignore_ascii_case("normal") {
                return Content::Normal;
            }
        }
    }
    let mut out = String::new();
    for c in comps {
        match c {
            Component::Str(s) => out.push_str(s),
            Component::Function { name, raw_args } if name == "attr" => {
                // Bare `attr(name)`; strip a stray quote pair. A typed/fallback
                // form (`attr(x px, 0)`) degrades to an empty value (§6).
                let attr_name = raw_args.trim().trim_matches('"').trim();
                let v = doc
                    .get_attribute(element, &attr_name.to_ascii_lowercase())
                    .unwrap_or("");
                out.push_str(v);
            }
            // Unsupported component anywhere → don't generate a box.
            _ => return Content::None,
        }
    }
    Content::Text(out)
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
        "font-style" => {
            if let Some(s) = font_style_of(comps) {
                style.font_style = s;
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

        // bidi / spaced / transformed text (E6-M3)
        "direction" => {
            if let Some(d) = direction_of(comps) {
                style.direction = d;
            }
        }
        "unicode-bidi" => {
            if let Some(u) = unicode_bidi_of(comps) {
                style.unicode_bidi = u;
            }
        }
        "letter-spacing" => {
            if let Some(v) = spacing_of(comps, em_basis, rem) {
                style.letter_spacing = v;
            }
        }
        "word-spacing" => {
            if let Some(v) = spacing_of(comps, em_basis, rem) {
                style.word_spacing = v;
            }
        }
        "text-transform" => {
            if let Some(t) = text_transform_of(comps) {
                style.text_transform = t;
            }
        }
        "white-space" => {
            if let Some(w) = white_space_of(comps) {
                style.white_space = w;
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

        "border-spacing" => apply_border_spacing(style, comps, em_basis, rem),
        "border-collapse" => {
            if let Some(bc) = border_collapse_of(comps) {
                style.border_collapse = bc;
            }
        }

        "grid-template-columns" => {
            if let Some(t) = track_list_of(comps, em_basis, rem) {
                style.grid_template_columns = t;
            }
        }
        "grid-template-rows" => {
            if let Some(t) = track_list_of(comps, em_basis, rem) {
                style.grid_template_rows = t;
            }
        }
        "grid-column" => {
            if let Some(g) = grid_line_shorthand(comps) {
                style.grid_column = g;
            }
        }
        "grid-row" => {
            if let Some(g) = grid_line_shorthand(comps) {
                style.grid_row = g;
            }
        }
        "grid-column-start" => style.grid_column.start = placement_of(comps),
        "grid-column-end" => style.grid_column.end = placement_of(comps),
        "grid-row-start" => style.grid_row.start = placement_of(comps),
        "grid-row-end" => style.grid_row.end = placement_of(comps),

        // grid alignment + areas (E5-M2)
        "justify-items" => {
            if let Some(a) = align_items_of(comps) {
                style.justify_items = a;
            }
        }
        "justify-self" => {
            if let Some(a) = align_self_of(comps) {
                style.justify_self = a;
            }
        }
        "align-content" => {
            if let Some(j) = justify_content_of(comps) {
                style.align_content = j;
            }
        }
        "grid-template-areas" => {
            if let Some(a) = grid_template_areas_of(comps) {
                style.grid_template_areas = a;
            }
        }
        "grid-area" => apply_grid_area(style, comps),

        // transforms (E5-M3)
        "transform" => {
            if let Some(t) = parse_transform(comps, em_basis, rem) {
                style.transform = t;
            }
        }
        "transform-origin" => {
            if let Some(o) = parse_transform_origin(comps, em_basis, rem) {
                style.transform_origin = o;
            }
        }
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

// --- transforms (E5-M3) ---

/// `none` → empty list; else parse each `Function` left-to-right. An
/// unrecognized / malformed function is skipped (lenient). Returns `None` (leave
/// unchanged) only when nothing parseable is present and it isn't `none`.
fn parse_transform(comps: &[Component], em: f32, rem: f32) -> Option<Vec<TransformFn>> {
    if let [Component::Keyword(k)] = comps {
        if k.eq_ignore_ascii_case("none") {
            return Some(Vec::new());
        }
    }
    let mut out = Vec::new();
    for c in comps {
        if let Component::Function { name, raw_args } = c {
            if let Some(f) = parse_transform_fn(name, raw_args, em, rem) {
                out.push(f);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// One `name(raw_args)` → a `TransformFn`. Args split on top-level commas.
fn parse_transform_fn(name: &str, raw: &str, em: f32, rem: f32) -> Option<TransformFn> {
    let args = split_top_level_commas(raw);
    match name.to_ascii_lowercase().as_str() {
        "translate" => {
            let x = parse_length_pct(args.first()?, em, rem)?;
            let y = match args.get(1) {
                Some(s) => parse_length_pct(s, em, rem)?,
                None => LengthPct::Px(0.0),
            };
            Some(TransformFn::Translate(x, y))
        }
        "translatex" => Some(TransformFn::Translate(
            parse_length_pct(args.first()?, em, rem)?,
            LengthPct::Px(0.0),
        )),
        "translatey" => Some(TransformFn::Translate(
            LengthPct::Px(0.0),
            parse_length_pct(args.first()?, em, rem)?,
        )),
        "scale" => {
            let sx = parse_num(args.first()?)?;
            let sy = match args.get(1) {
                Some(s) => parse_num(s)?,
                None => sx,
            };
            Some(TransformFn::Scale(sx, sy))
        }
        "scalex" => Some(TransformFn::Scale(parse_num(args.first()?)?, 1.0)),
        "scaley" => Some(TransformFn::Scale(1.0, parse_num(args.first()?)?)),
        "rotate" => Some(TransformFn::Rotate(parse_angle_rad(args.first()?)?)),
        "skew" => {
            let ax = parse_angle_rad(args.first()?)?;
            let ay = match args.get(1) {
                Some(s) => parse_angle_rad(s)?,
                None => 0.0,
            };
            Some(TransformFn::Skew(ax, ay))
        }
        "skewx" => Some(TransformFn::Skew(parse_angle_rad(args.first()?)?, 0.0)),
        "skewy" => Some(TransformFn::Skew(0.0, parse_angle_rad(args.first()?)?)),
        "matrix" => {
            if args.len() != 6 {
                return None;
            }
            let mut m = [0.0f32; 6];
            for (i, a) in args.iter().enumerate() {
                m[i] = parse_num(a)?;
            }
            Some(TransformFn::Matrix(m))
        }
        // matrix3d / translate3d / perspective / … deferred (E5-M3 §5)
        _ => None,
    }
}

/// `<number>` (bare): "2", "1.5", "-0.5".
fn parse_num(s: &str) -> Option<f32> {
    s.trim().parse::<f32>().ok()
}

/// `<length-percentage>`: "20px"|"%"|"em"|"rem"; a bare "0" → Px(0).
fn parse_length_pct(s: &str, em: f32, rem: f32) -> Option<LengthPct> {
    let t = s.trim();
    if let Some(p) = t.strip_suffix('%') {
        return p.trim().parse().ok().map(LengthPct::Percent);
    }
    if let Some(p) = t.strip_suffix("px") {
        return p.trim().parse().ok().map(LengthPct::Px);
    }
    if let Some(p) = t.strip_suffix("rem") {
        return p.trim().parse().ok().map(|v: f32| LengthPct::Px(v * rem));
    }
    if let Some(p) = t.strip_suffix("em") {
        return p.trim().parse().ok().map(|v: f32| LengthPct::Px(v * em));
    }
    if t == "0" {
        return Some(LengthPct::Px(0.0));
    }
    None
}

/// `<angle>` → RADIANS. deg/rad/turn/grad. (`grad`/`turn` tested before `rad`.)
fn parse_angle_rad(s: &str) -> Option<f32> {
    let t = s.trim().to_ascii_lowercase();
    let pick = |suf: &str| t.strip_suffix(suf).and_then(|n| n.trim().parse::<f32>().ok());
    if let Some(v) = pick("grad") {
        return Some(v * std::f32::consts::PI / 200.0);
    }
    if let Some(v) = pick("turn") {
        return Some(v * std::f32::consts::TAU);
    }
    if let Some(v) = pick("rad") {
        return Some(v);
    }
    if let Some(v) = pick("deg") {
        return Some(v.to_radians());
    }
    if t == "0" {
        return Some(0.0);
    }
    None
}

/// `transform-origin: <x> [<y>]?`. Keyword/length per axis. Default y = center.
/// 3rd (z) value ignored; keyword order not disambiguated (first = x). (§5)
fn parse_transform_origin(
    comps: &[Component],
    em: f32,
    rem: f32,
) -> Option<(LengthPct, LengthPct)> {
    let mut xs: Vec<LengthPct> = Vec::new();
    for c in comps {
        match c {
            Component::Dimension { value, unit } => match unit.as_str() {
                "px" => xs.push(LengthPct::Px(*value)),
                "%" => xs.push(LengthPct::Percent(*value)),
                "em" => xs.push(LengthPct::Px(*value * em)),
                "rem" => xs.push(LengthPct::Px(*value * rem)),
                _ => {}
            },
            Component::Number(n) if *n == 0.0 => xs.push(LengthPct::Px(0.0)),
            Component::Keyword(k) => match k.to_ascii_lowercase().as_str() {
                "left" | "top" => xs.push(LengthPct::Percent(0.0)),
                "right" | "bottom" => xs.push(LengthPct::Percent(100.0)),
                "center" => xs.push(LengthPct::Percent(50.0)),
                _ => {}
            },
            _ => {}
        }
        if xs.len() == 2 {
            break;
        }
    }
    match xs.as_slice() {
        [x] => Some((*x, LengthPct::Percent(50.0))),
        [x, y] => Some((*x, *y)),
        _ => None,
    }
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
            "grid" => Some(Display::Grid),
            "inline-grid" => Some(Display::InlineGrid),
            "table" => Some(Display::Table),
            "inline-table" => Some(Display::InlineTable),
            "table-row-group" => Some(Display::TableRowGroup),
            "table-header-group" => Some(Display::TableRowGroup),
            "table-footer-group" => Some(Display::TableRowGroup),
            "table-row" => Some(Display::TableRow),
            "table-cell" => Some(Display::TableCell),
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
            "flex-start" | "start" | "left" => Some(AlignItems::FlexStart),
            "flex-end" | "end" | "right" => Some(AlignItems::FlexEnd),
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
            "flex-start" | "start" | "left" => Some(AlignSelf::FlexStart),
            "flex-end" | "end" | "right" => Some(AlignSelf::FlexEnd),
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

/// `border-spacing: <length> [<length>]`. One value → both axes; two → (h, v).
/// `auto`/percent are invalid here → ignored. (E7-M3)
fn apply_border_spacing(style: &mut ComputedStyle, comps: &[Component], em_basis: f32, rem: f32) {
    let mut vals = Vec::with_capacity(2);
    for c in comps {
        if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem) {
            vals.push(px.max(0.0));
        }
    }
    match vals.as_slice() {
        [h] => style.border_spacing = (*h, *h),
        [h, v, ..] => style.border_spacing = (*h, *v),
        [] => {}
    }
}

fn border_collapse_of(comps: &[Component]) -> Option<BorderCollapse> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "separate" => Some(BorderCollapse::Separate),
            "collapse" => Some(BorderCollapse::Collapse),
            _ => None,
        },
        _ => None,
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

/// `font-style: normal | italic | oblique [<angle>]`. `oblique` with any
/// trailing angle folds to `Oblique` (angle ignored).
fn font_style_of(comps: &[Component]) -> Option<FontStyle> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "normal" => Some(FontStyle::Normal),
            "italic" => Some(FontStyle::Italic),
            "oblique" => Some(FontStyle::Oblique),
            _ => None,
        },
        // `oblique 14deg` → keyword + dimension; treat as Oblique.
        [Component::Keyword(k), ..] if k.eq_ignore_ascii_case("oblique") => Some(FontStyle::Oblique),
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

fn direction_of(comps: &[Component]) -> Option<Direction> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "ltr" => Some(Direction::Ltr),
            "rtl" => Some(Direction::Rtl),
            _ => None,
        },
        _ => None,
    }
}

fn unicode_bidi_of(comps: &[Component]) -> Option<UnicodeBidi> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "normal" => Some(UnicodeBidi::Normal),
            "embed" => Some(UnicodeBidi::Embed),
            "bidi-override" => Some(UnicodeBidi::BidiOverride),
            "isolate" => Some(UnicodeBidi::Isolate),
            // fold the un-modelled values to their nearest modelled value.
            "isolate-override" => Some(UnicodeBidi::BidiOverride),
            "plaintext" => Some(UnicodeBidi::Normal),
            _ => None,
        },
        _ => None,
    }
}

/// `letter-spacing` / `word-spacing`: `<length>` (px/em/rem → px) or `normal` → 0.
/// `%` is out of scope → `None` (ignored).
fn spacing_of(comps: &[Component], em: f32, rem: f32) -> Option<f32> {
    match comps {
        [Component::Keyword(k)] if k.eq_ignore_ascii_case("normal") => Some(0.0),
        _ => as_px_with(comps, em, rem),
    }
}

fn text_transform_of(comps: &[Component]) -> Option<TextTransform> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "none" => Some(TextTransform::None),
            "uppercase" => Some(TextTransform::Uppercase),
            "lowercase" => Some(TextTransform::Lowercase),
            "capitalize" => Some(TextTransform::Capitalize),
            _ => None,
        },
        _ => None,
    }
}

fn white_space_of(comps: &[Component]) -> Option<WhiteSpace> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "normal" => Some(WhiteSpace::Normal),
            "pre" => Some(WhiteSpace::Pre),
            "nowrap" => Some(WhiteSpace::Nowrap),
            "pre-wrap" => Some(WhiteSpace::PreWrap),
            "pre-line" => Some(WhiteSpace::PreLine),
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

// --- E5-M1: grid track lists + line placement ---

/// Parse a `grid-template-columns`/`-rows` track list. `None` (declaration
/// ignored) on an empty / `none` / unsupported (`auto-fill`/`minmax`) value.
fn track_list_of(comps: &[Component], em_basis: f32, rem: f32) -> Option<Vec<TrackSize>> {
    // `none` → empty list (no explicit tracks).
    if let [Component::Keyword(k)] = comps {
        if k.eq_ignore_ascii_case("none") {
            return Some(Vec::new());
        }
    }
    let mut out: Vec<TrackSize> = Vec::new();
    for c in comps {
        match c {
            Component::Function { name, raw_args } if name.eq_ignore_ascii_case("repeat") => {
                expand_repeat(raw_args, em_basis, rem, &mut out)?;
            }
            _ => {
                let t = track_size_of_component(c, em_basis, rem)?; // unknown → whole list fails
                out.push(t);
            }
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// One track component → `TrackSize`. Recognizes px/em/rem (→Px), %, fr, auto.
fn track_size_of_component(c: &Component, em_basis: f32, rem: f32) -> Option<TrackSize> {
    match c {
        Component::Dimension { value, unit } => match unit.as_str() {
            "px" => Some(TrackSize::Px(*value)),
            "em" => Some(TrackSize::Px(*value * em_basis)),
            "rem" => Some(TrackSize::Px(*value * rem)),
            "%" => Some(TrackSize::Percent(*value)),
            "fr" => Some(TrackSize::Fr(value.max(0.0))),
            _ => None,
        },
        // `0` (a bare Number) is a valid `0px` track.
        Component::Number(n) if *n == 0.0 => Some(TrackSize::Px(0.0)),
        Component::Keyword(k) if k.eq_ignore_ascii_case("auto") => Some(TrackSize::Auto),
        _ => None,
    }
}

/// One raw track token (`100px`, `1fr`, `auto`, `50%`, `0`) → `TrackSize`.
fn track_size_of_token(tok: &str, em_basis: f32, rem: f32) -> Option<TrackSize> {
    let t = tok.trim();
    if t.eq_ignore_ascii_case("auto") {
        return Some(TrackSize::Auto);
    }
    if t == "0" {
        return Some(TrackSize::Px(0.0));
    }
    if let Some(n) = t.strip_suffix("px") {
        return n.trim().parse::<f32>().ok().map(TrackSize::Px);
    }
    if let Some(n) = t.strip_suffix("fr") {
        return n.trim().parse::<f32>().ok().map(|v| TrackSize::Fr(v.max(0.0)));
    }
    if let Some(n) = t.strip_suffix('%') {
        return n.trim().parse::<f32>().ok().map(TrackSize::Percent);
    }
    if let Some(n) = t.strip_suffix("rem") {
        return n.trim().parse::<f32>().ok().map(|v| TrackSize::Px(v * rem));
    }
    if let Some(n) = t.strip_suffix("em") {
        return n.trim().parse::<f32>().ok().map(|v| TrackSize::Px(v * em_basis));
    }
    None
}

/// Expand `repeat(<int>, <tracklist>)`. `raw_args` is the verbatim inner text,
/// e.g. `"3, 1fr"` or `"2, 100px 1fr"`. We split on the first comma (the count),
/// then split the rest on whitespace and parse each token. `auto-fill`/`auto-fit`
/// (non-integer count) → `None` (drop the declaration).
fn expand_repeat(raw_args: &str, em_basis: f32, rem: f32, out: &mut Vec<TrackSize>) -> Option<()> {
    let (count_str, rest) = raw_args.split_once(',')?;
    let n: usize = count_str.trim().parse().ok()?; // non-integer (auto-fill) → None
    if n == 0 || n > 1000 {
        return None; // guard absurd counts
    }
    let mut one: Vec<TrackSize> = Vec::new();
    for tok in rest.split_ascii_whitespace() {
        one.push(track_size_of_token(tok, em_basis, rem)?);
    }
    if one.is_empty() {
        return None;
    }
    for _ in 0..n {
        out.extend(one.iter().copied());
    }
    Some(())
}

/// `grid-column`/`grid-row` shorthand → a `{start, end}` pair. Splits the
/// component list on the `/` Raw token; each side parsed as a placement. A
/// missing end side → `Auto`. Returns None if a non-empty value parses to no
/// placement at all.
fn grid_line_shorthand(comps: &[Component]) -> Option<GridLine> {
    let mut sides = comps.split(|c| matches!(c, Component::Raw(s) if s == "/"));
    let start = placement_of(sides.next().unwrap_or(&[]));
    let end = placement_of(sides.next().unwrap_or(&[]));
    if start == GridPlacement::Auto && end == GridPlacement::Auto && !comps.is_empty() {
        return None; // value present but unrecognized → ignore declaration
    }
    Some(GridLine { start, end })
}

/// One side: `auto` | `<integer>` | `span <integer>`. Defaults to `Auto`.
fn placement_of(comps: &[Component]) -> GridPlacement {
    match comps {
        [Component::Keyword(k)] if k.eq_ignore_ascii_case("auto") => GridPlacement::Auto,
        // bare line number (parser yields `Number` for an unitless integer)
        [Component::Number(n)] => {
            let i = *n as i32;
            if i == 0 {
                GridPlacement::Auto
            } else {
                GridPlacement::Line(i)
            }
        }
        // `span N`
        [Component::Keyword(k), Component::Number(n)] if k.eq_ignore_ascii_case("span") => {
            GridPlacement::Span((*n as i32).max(1) as u32)
        }
        _ => GridPlacement::Auto,
    }
}

/// Parse `grid-template-areas`: a list of quoted row strings → a 2D name grid.
/// `none` → `Some(Vec::new())` (clears the grid). Each name lowercased; a run of
/// `.` becomes the empty marker `"."`. Returns `None` when no row strings are
/// present (declaration has no effect).
fn grid_template_areas_of(comps: &[Component]) -> Option<Vec<Vec<String>>> {
    if let [Component::Keyword(k)] = comps {
        if k.eq_ignore_ascii_case("none") {
            return Some(Vec::new());
        }
    }
    let mut rows: Vec<Vec<String>> = Vec::new();
    for c in comps {
        if let Component::Str(s) = c {
            let row: Vec<String> = s
                .split_ascii_whitespace()
                .map(|tok| {
                    if tok.chars().all(|ch| ch == '.') {
                        ".".to_string()
                    } else {
                        tok.to_ascii_lowercase()
                    }
                })
                .collect();
            rows.push(row);
        }
    }
    if rows.is_empty() {
        return None;
    }
    Some(rows)
}

/// `grid-area` shorthand. Either `<name>` (a named-area ref → `grid_area_name`)
/// or `r-start / c-start / r-end / c-end` (→ grid_row + grid_column). A bare
/// single ident that is not `auto`/`span` is treated as the name form.
fn apply_grid_area(style: &mut ComputedStyle, comps: &[Component]) {
    let has_slash = comps.iter().any(|c| matches!(c, Component::Raw(s) if s == "/"));
    if !has_slash {
        if let [Component::Keyword(k)] = comps {
            let lk = k.to_ascii_lowercase();
            if lk != "auto" && lk != "span" {
                style.grid_area_name = Some(lk);
            }
        }
        return;
    }
    // 4-line form: split on `/` into up to four sides.
    let mut sides = comps.split(|c| matches!(c, Component::Raw(s) if s == "/"));
    let rs = placement_of(sides.next().unwrap_or(&[]));
    let cs = placement_of(sides.next().unwrap_or(&[]));
    let re = placement_of(sides.next().unwrap_or(&[]));
    let ce = placement_of(sides.next().unwrap_or(&[]));
    style.grid_row = GridLine { start: rs, end: re };
    style.grid_column = GridLine { start: cs, end: ce };
    style.grid_area_name = None;
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
