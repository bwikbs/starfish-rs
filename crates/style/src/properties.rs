//! Declaration → typed field application (§5). Reuses M2's typed components.

use std::collections::HashMap;

use starfish_css::{parse_component_values, Component, Declaration, Rgba};
use starfish_dom::{Document, NodeId};

use crate::computed::{
    AlignItems, AlignSelf, AnimDirection, AnimFillMode, Animation, BackgroundLayer, BgImage,
    BgRepeat, BgSize, BgSizeAxis, BlendMode, BorderCollapse, BorderStyle, BoxShadow, BoxSizing,
    Clear, ComputedStyle, ConicGradient, ContainerType, Content, Direction, Display, Easing,
    FilterFn,
    FlexDirection, FlexWrap, Float, FontStyle, GradientStop, GridLine, GridPlacement, Hyphens,
    ImageRendering, JumpTerm, JustifyContent, Length, LengthPct, LineHeight, LinearGradient,
    ListStylePosition, ListStyleType, MaskImage, MaskMode, MaskSpec, ObjectFit, Overflow,
    OverflowWrap, Position, RadialGradient, TabSize, TextAlign, TextDecorationLine, TextJustify,
    TextOrientation, TextOverflow, TextShadow, TextTransform, TrackSize, TransformFn, Transition,
    TransitionProp, UnicodeBidi, WhiteSpace, WordBreak, WritingMode,
};
use crate::counters::{format_counter, parse_counter_args, parse_counters_args, CounterState};
use crate::Viewport;

const TRANSPARENT: Rgba = Rgba {
    r: 0,
    g: 0,
    b: 0,
    a: 0,
};

/// Per-element resolution context for `em`/`rem`/viewport units.
#[derive(Clone, Copy)]
pub(crate) struct EmContext {
    /// Parent's computed font-size (basis for `em` on `font-size`).
    pub parent_font_size: f32,
    /// Root element's computed font-size (basis for `rem`).
    pub root_font_size: f32,
    /// Render viewport (basis for `vw`/`vh`/`vmin`/`vmax`) (E13-M3).
    pub viewport: crate::Viewport,
}

/// Resolve a `content` declaration's value to a [`Content`], given the
/// originating element so `attr()` can look up an attribute (E7-M2). Grammar:
/// `none`/`normal` → no box; `<string>+` / `attr(name)` / their concatenation →
/// `Text`; any unsupported component (counter/url/quote/…) → `None` (no box).
pub(crate) fn resolve_content(
    doc: &Document,
    element: NodeId,
    decl: &Declaration,
    counters: &CounterState,
) -> Content {
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
            // CSS counters (E16-M1): `counter(name[, style])`.
            Component::Function { name, raw_args } if name == "counter" => {
                let (n, sty) = parse_counter_args(raw_args);
                out.push_str(&format_counter(counters.value(&n), sty));
            }
            // `counters(name, "sep"[, style])`: join the whole nesting stack.
            Component::Function { name, raw_args } if name == "counters" => {
                let (n, sep, sty) = parse_counters_args(raw_args);
                let joined = counters
                    .stack(&n)
                    .iter()
                    .map(|&v| format_counter(v, sty))
                    .collect::<Vec<_>>()
                    .join(&sep);
                out.push_str(&joined);
            }
            // Unsupported component anywhere → don't generate a box.
            _ => return Content::None,
        }
    }
    Content::Text(out)
}

/// Substitute every `attr(name <type>?, fallback?)` in `decl`'s value with the
/// concrete value read from `element`'s attributes (E24-M3), returning a new
/// owned declaration. Returns `None` when there is no `attr()` to expand (the
/// common, byte-identical path — the caller keeps borrowing the original).
///
/// The `content` property is NOT routed through here; it keeps its own
/// string-only `attr()` handling in [`resolve_content`].
pub(crate) fn substitute_attr_decl(
    decl: &Declaration,
    doc: &Document,
    element: NodeId,
) -> Option<Declaration> {
    let comps = &decl.value.components;
    let has_attr = comps
        .iter()
        .any(|c| matches!(c, Component::Function { name, .. } if name.eq_ignore_ascii_case("attr")));
    if !has_attr {
        return None;
    }
    let new_comps = comps
        .iter()
        .map(|c| match c {
            Component::Function { name, raw_args } if name.eq_ignore_ascii_case("attr") => {
                resolve_attr_component(raw_args, doc, element)
            }
            other => other.clone(),
        })
        .collect();
    Some(Declaration {
        name: decl.name.clone(),
        value: starfish_css::Value {
            raw: decl.value.raw.clone(),
            components: new_comps,
        },
        important: decl.important,
    })
}

/// Resolve one `attr(name <type>?, fallback?)` to a concrete component. The
/// attribute value is typed per the optional unit/keyword; on a missing or
/// unparseable attribute the fallback (or the type's default — 0 / empty) wins.
fn resolve_attr_component(raw_args: &str, doc: &Document, element: NodeId) -> Component {
    let (head, fallback) = match split_first_top_comma(raw_args) {
        Some(fb) => (&raw_args[..raw_args.len() - fb.len() - 1], Some(fb.trim())),
        None => (raw_args, None),
    };
    let mut it = head.split_whitespace();
    let name = it.next().unwrap_or("").trim_matches('"');
    let ty = it.next().map(|s| s.to_ascii_lowercase());
    let ty = ty.as_deref();

    if let Some(val) = doc.get_attribute(element, &name.to_ascii_lowercase()) {
        if let Some(c) = typed_attr_component(val.trim(), ty) {
            return c;
        }
    }
    if let Some(fb) = fallback {
        if !fb.is_empty() {
            if let Some(c) = starfish_css::parse_component_values(fb).into_iter().next() {
                return c;
            }
        }
    }
    attr_default_component(ty)
}

/// Type an attribute's string value by the `attr()` type token. `None` if the
/// value can't be parsed for a numeric type (caller then uses the fallback).
fn typed_attr_component(val: &str, ty: Option<&str>) -> Option<Component> {
    match ty {
        None | Some("string") => Some(Component::Str(val.to_string())),
        Some("number") | Some("integer") => val.parse::<f32>().ok().map(Component::Number),
        Some(unit) => val.parse::<f32>().ok().map(|value| Component::Dimension {
            value,
            unit: unit.to_string(),
        }),
    }
}

/// The `attr()` missing-value default for a type: 0 for numeric/dimension,
/// empty string otherwise.
fn attr_default_component(ty: Option<&str>) -> Component {
    match ty {
        None | Some("string") => Component::Str(String::new()),
        Some("number") | Some("integer") => Component::Number(0.0),
        Some(unit) => Component::Dimension {
            value: 0.0,
            unit: unit.to_string(),
        },
    }
}

/// Whether the engine "supports" `decl`, for `@supports` evaluation (E24-M2).
/// Custom properties (`--*`) are supported iff non-empty; a value containing
/// `var()` is assumed supported (it can't be validated without an element).
/// Everything else uses a change-detection probe: apply the declaration onto a
/// fresh initial style and see whether anything changed.
///
/// KNOWN LIMITATION: a supported value that equals the initial value (e.g.
/// `display: inline`) reads as unsupported — acceptable for this milestone.
pub(crate) fn declaration_supported(decl: &Declaration, vp: Viewport) -> bool {
    if decl.name.starts_with("--") {
        return !decl.value.raw.is_empty();
    }
    if has_var(&decl.value.components) {
        return true;
    }
    let before = ComputedStyle::initial();
    let mut probe = before.clone();
    let ctx = EmContext {
        parent_font_size: 16.0,
        root_font_size: 16.0,
        viewport: vp,
    };
    // NOTE: apply_declaration's bool return is border-color-specific — the
    // probe compares whole styles instead.
    apply_declaration(&mut probe, decl, ctx, &HashMap::new());
    probe != before
}

/// Apply one declaration onto `style`. Returns `true` if it explicitly set the
/// border color (so the cascade can keep currentColor otherwise). Unknown
/// properties / unparseable values are ignored (lenient, never panics).
pub(crate) fn apply_declaration(
    style: &mut ComputedStyle,
    decl: &Declaration,
    ctx: EmContext,
    custom: &HashMap<String, Vec<Component>>,
) -> bool {
    // var() substitution (E13-M2). Only clone+substitute when a `var()` is
    // actually present, so non-var declarations stay on the byte-identical
    // fast path (the original components are borrowed). An unresolved var with
    // no fallback (or a cycle) makes the whole declaration invalid → skip.
    let substituted;
    let comps: &[Component] = if has_var(&decl.value.components) {
        match substitute_vars(&decl.value.components, custom, 0) {
            Some(v) => {
                substituted = v;
                &substituted
            }
            None => return false,
        }
    } else {
        &decl.value.components
    };
    // `em` on non-font-size lengths resolves against this element's own,
    // already-applied font-size (font-size is applied first; see cascade).
    let em_basis = style.font_size;
    let rem = ctx.root_font_size;
    let vp = ctx.viewport;

    match decl.name.as_str() {
        "display" => {
            if let Some(d) = as_display(comps) {
                style.display = d;
            }
        }
        "width" => set_len(comps, em_basis, rem, vp, &mut style.width),
        "height" => set_len(comps, em_basis, rem, vp, &mut style.height),

        // box-sizing + min/max sizing (E13-M1).
        "box-sizing" => {
            if let Some(bs) = box_sizing_of(comps) {
                style.box_sizing = bs;
            }
        }
        "aspect-ratio" => set_aspect_ratio(comps, &mut style.aspect_ratio),
        "min-width" => set_len(comps, em_basis, rem, vp, &mut style.min_width),
        "min-height" => set_len(comps, em_basis, rem, vp, &mut style.min_height),
        "max-width" => set_max_len(comps, em_basis, rem, vp, &mut style.max_width),
        "max-height" => set_max_len(comps, em_basis, rem, vp, &mut style.max_height),

        "margin-top" => set_len(comps, em_basis, rem, vp, &mut style.margin_top),
        "margin-right" => set_len(comps, em_basis, rem, vp, &mut style.margin_right),
        "margin-bottom" => set_len(comps, em_basis, rem, vp, &mut style.margin_bottom),
        "margin-left" => set_len(comps, em_basis, rem, vp, &mut style.margin_left),
        "margin" => {
            if let Some([t, r, b, l]) = shorthand_lengths(comps, em_basis, rem, vp, false) {
                style.margin_top = t;
                style.margin_right = r;
                style.margin_bottom = b;
                style.margin_left = l;
            }
        }

        "padding-top" => set_len(comps, em_basis, rem, vp, &mut style.padding_top),
        "padding-right" => set_len(comps, em_basis, rem, vp, &mut style.padding_right),
        "padding-bottom" => set_len(comps, em_basis, rem, vp, &mut style.padding_bottom),
        "padding-left" => set_len(comps, em_basis, rem, vp, &mut style.padding_left),
        "padding" => {
            if let Some([t, r, b, l]) = shorthand_lengths(comps, em_basis, rem, vp, true) {
                style.padding_top = t;
                style.padding_right = r;
                style.padding_bottom = b;
                style.padding_left = l;
            }
        }

        "border-top-width" => set_px(comps, em_basis, rem, vp, &mut style.border_top_width),
        "border-right-width" => set_px(comps, em_basis, rem, vp, &mut style.border_right_width),
        "border-bottom-width" => set_px(comps, em_basis, rem, vp, &mut style.border_bottom_width),
        "border-left-width" => set_px(comps, em_basis, rem, vp, &mut style.border_left_width),
        "border-width" => {
            if let Some([t, r, b, l]) = shorthand_px(comps, em_basis, rem, vp) {
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
        "border" => return apply_border_shorthand(style, comps, em_basis, rem, vp),

        "color" => {
            if let Some(c) = as_color(comps) {
                style.color = c;
            }
        }
        "background-color" => {
            if let Some(c) = first_color(comps) {
                style.background_color = c;
            }
        }
        "background-image" => {
            style.background_layers = parse_bg_image_list(comps);
        }
        "background-size" => {
            apply_bg_sizes(
                &mut style.background_layers,
                parse_bg_size_list(comps, em_basis, rem, vp),
            );
        }
        "background-position" => {
            apply_bg_positions(
                &mut style.background_layers,
                parse_bg_position_list(comps, em_basis, rem, vp),
            );
        }
        "background-repeat" => {
            apply_bg_repeats(&mut style.background_layers, parse_bg_repeat_list(comps));
        }
        "background" => apply_background_shorthand(style, comps, em_basis, rem, vp),
        "border-radius" => {
            if let Some(r) = border_radius_shorthand(comps, em_basis, rem, vp) {
                style.border_radius = r;
            }
        }
        "box-shadow" => {
            if let Some(s) = parse_box_shadow(comps, em_basis, rem, vp) {
                style.box_shadow = Some(s);
            }
        }
        "text-shadow" => {
            // `none` → clear; a parsed shadow → set. An unparseable value leaves
            // the inherited/current value (lenient).
            if let [Component::Keyword(k)] = comps {
                if k.eq_ignore_ascii_case("none") {
                    style.text_shadow = None;
                }
            }
            if let Some(s) = parse_text_shadow(comps, em_basis, rem, vp) {
                style.text_shadow = Some(s);
            }
        }

        "outline-width" => {
            if let Some(px) =
                as_px_with(comps, em_basis, rem, vp).or_else(|| border_width_keyword(comps))
            {
                style.outline.width = px;
            }
        }
        "outline-style" => {
            if let [Component::Keyword(k)] = comps {
                // `auto` (focus-ring) → Solid; otherwise reuse the border helper.
                if k.eq_ignore_ascii_case("auto") {
                    style.outline.style = BorderStyle::Solid;
                } else {
                    style.outline.style = style_keyword(k);
                }
            }
        }
        "outline-color" => {
            if let Some(c) = first_color(comps) {
                style.outline.color = c;
            }
        }
        "outline-offset" => {
            if let Some(px) = as_px_with(comps, em_basis, rem, vp) {
                style.outline.offset = px;
            }
        }
        "outline" => apply_outline_shorthand(style, comps, em_basis, rem, vp),
        "opacity" => {
            if let Some(n) = single_number(comps) {
                style.opacity = n.clamp(0.0, 1.0);
            } else if let Some(p) = single_percent(comps) {
                style.opacity = (p / 100.0).clamp(0.0, 1.0);
            }
        }

        "font-size" => {
            // `em`/`%` on font-size resolve against the *parent* font-size.
            // Math functions (calc/min/max/clamp/…) resolve their percent part
            // against the parent font-size — a definite basis — into a plain px
            // (font-size never holds a Calc/Math).
            if let [Component::Function { name, raw_args }] = comps {
                if crate::calc::is_math_fn(name) {
                    if let Some(l) =
                        crate::calc::eval_math_fn(name, raw_args, ctx.parent_font_size, rem, vp)
                    {
                        match l {
                            Length::Px(v) => style.font_size = v,
                            Length::Percent(p) => {
                                style.font_size = p / 100.0 * ctx.parent_font_size
                            }
                            Length::Calc { px, percent } => {
                                style.font_size = px + percent / 100.0 * ctx.parent_font_size
                            }
                            Length::Math(m) => style.font_size = m.resolve(ctx.parent_font_size),
                            Length::Auto => {}
                        }
                    }
                    return false;
                }
            }
            if let Some(px) = as_px_with(comps, ctx.parent_font_size, rem, vp) {
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
            if let Some(lh) = line_height_of(comps, em_basis, rem, vp) {
                style.line_height = lh;
            }
        }
        "text-align" => {
            if let Some(a) = text_align_of(comps) {
                style.text_align = a;
            }
        }
        "text-indent" => {
            if let Some(v) = text_indent_of(comps, em_basis, rem, vp) {
                style.text_indent = v;
            }
        }
        "text-justify" => {
            if let Some(j) = text_justify_of(comps) {
                style.text_justify = j;
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
            if let Some(v) = spacing_of(comps, em_basis, rem, vp) {
                style.letter_spacing = v;
            }
        }
        "word-spacing" => {
            if let Some(v) = spacing_of(comps, em_basis, rem, vp) {
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
        "word-break" => {
            if let Some(w) = word_break_of(comps) {
                style.word_break = w;
            }
        }
        "overflow-wrap" | "word-wrap" => {
            if let Some(w) = overflow_wrap_of(comps) {
                style.overflow_wrap = w;
            }
        }
        "tab-size" => {
            if let Some(t) = tab_size_of(comps, em_basis, rem, vp) {
                style.tab_size = t;
            }
        }
        "hyphens" | "-webkit-hyphens" => {
            if let Some(h) = hyphens_of(comps) {
                style.hyphens = h;
            }
        }
        "-webkit-line-clamp" | "line-clamp" => {
            style.line_clamp = line_clamp_of(comps);
        }
        // `-webkit-box-orient` is accepted and ignored (line-clamp MVP).
        "-webkit-box-orient" => {}

        // writing mode (E18-M3)
        "writing-mode" => {
            if let Some(w) = writing_mode_of(comps) {
                style.writing_mode = w;
            }
        }
        "text-orientation" => {
            if let Some(o) = text_orientation_of(comps) {
                style.text_orientation = o;
            }
        }

        // container queries (E25-M1)
        "container-type" => {
            if let Some(t) = container_type_of(comps) {
                style.container_type = t;
            }
        }
        "container-name" => {
            style.container_name = container_name_of(comps);
        }
        // `container: <name> [/ <type>]` shorthand.
        "container" => apply_container_shorthand(style, comps),

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
        // overflow / overflow-x / overflow-y all take the first keyword (E13-M4).
        "overflow" | "overflow-x" | "overflow-y" => {
            if let Some(o) = overflow_of(comps) {
                style.overflow = o;
            }
        }
        "text-overflow" => {
            if let Some(t) = text_overflow_of(comps) {
                style.text_overflow = t;
            }
        }
        "top" => set_len(comps, em_basis, rem, vp, &mut style.top),
        "right" => set_len(comps, em_basis, rem, vp, &mut style.right),
        "bottom" => set_len(comps, em_basis, rem, vp, &mut style.bottom),
        "left" => set_len(comps, em_basis, rem, vp, &mut style.left),

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
        "flex-basis" => set_len(comps, em_basis, rem, vp, &mut style.flex_basis),
        "flex" => apply_flex_shorthand(style, comps, em_basis, rem, vp),

        "row-gap" => set_len_no_auto(comps, em_basis, rem, vp, &mut style.row_gap),
        "column-gap" => set_len_no_auto(comps, em_basis, rem, vp, &mut style.column_gap),
        "gap" => apply_gap_shorthand(style, comps, em_basis, rem, vp),

        // multi-column (E18-M2)
        "column-count" => {
            if let [Component::Keyword(k)] = comps {
                if k.eq_ignore_ascii_case("auto") {
                    style.column_count = None;
                }
            } else if let Some(n) = single_number(comps) {
                if n >= 1.0 {
                    style.column_count = Some(n.floor() as u32);
                }
            }
        }
        "column-width" => {
            if let [Component::Keyword(k)] = comps {
                if k.eq_ignore_ascii_case("auto") {
                    style.column_width = None;
                }
            } else if let Some(l) = as_length(comps, em_basis, rem, vp) {
                style.column_width = Some(l);
            }
        }
        "column-rule-width" => {
            if let Some(px) =
                as_px_with(comps, em_basis, rem, vp).or_else(|| border_width_keyword(comps))
            {
                style.column_rule_width = px;
            }
        }
        "column-rule-style" => {
            if let [Component::Keyword(k)] = comps {
                style.column_rule_style = style_keyword(k);
            }
        }
        "column-rule-color" => {
            if let Some(c) = first_color(comps) {
                style.column_rule_color = c;
            }
        }
        "columns" => apply_columns_shorthand(style, comps, em_basis, rem, vp),
        "column-rule" => apply_column_rule_shorthand(style, comps, em_basis, rem, vp),

        "border-spacing" => apply_border_spacing(style, comps, em_basis, rem, vp),
        "border-collapse" => {
            if let Some(bc) = border_collapse_of(comps) {
                style.border_collapse = bc;
            }
        }

        "grid-template-columns" => {
            if let Some(t) = track_list_of(comps, em_basis, rem, vp) {
                style.grid_template_columns = t;
            }
        }
        "grid-template-rows" => {
            if let Some(t) = track_list_of(comps, em_basis, rem, vp) {
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
            if let Some(t) = parse_transform(comps, em_basis, rem, vp) {
                style.transform = t;
            }
        }
        "transform-origin" => {
            if let Some(o) = parse_transform_origin(comps, em_basis, rem, vp) {
                style.transform_origin = o;
            }
        }
        // filter (E21-M1)
        "filter" => {
            if let Some(f) = parse_filter(comps) {
                style.filter = f;
            }
        }
        // blend modes (E21-M2)
        "mix-blend-mode" => {
            if let [Component::Keyword(k)] = comps {
                if let Some(m) = blend_mode_kw(k) {
                    style.mix_blend_mode = m;
                }
            }
        }
        "background-blend-mode" => {
            // One mode per comma group; each group is a single keyword.
            let modes: Vec<BlendMode> = comps
                .split(|c| matches!(c, Component::Comma))
                .filter_map(|seg| match seg {
                    [Component::Keyword(k)] => blend_mode_kw(k),
                    _ => None,
                })
                .collect();
            if !modes.is_empty() {
                style.background_blend_mode = modes;
            }
        }

        // masking + backdrop-filter (E21-M3)
        "mask-image" | "-webkit-mask-image" => {
            style.mask = parse_mask_image(comps);
        }
        "mask-mode" => {
            if let (Some(m), [Component::Keyword(k)]) = (style.mask.as_mut(), comps) {
                if k.eq_ignore_ascii_case("alpha") {
                    m.mode = MaskMode::Alpha;
                } else if k.eq_ignore_ascii_case("luminance") {
                    m.mode = MaskMode::Luminance;
                }
            }
        }
        "mask-size" => {
            if let Some(m) = style.mask.as_mut() {
                if let Some(s) = parse_bg_size_list(comps, em_basis, rem, vp)
                    .into_iter()
                    .next()
                {
                    m.size = s;
                }
            }
        }
        "mask-position" => {
            if let Some(m) = style.mask.as_mut() {
                if let Some(p) = parse_bg_position_list(comps, em_basis, rem, vp)
                    .into_iter()
                    .next()
                {
                    m.position = p;
                }
            }
        }
        "mask-repeat" => {
            if let Some(m) = style.mask.as_mut() {
                if let Some(r) = parse_bg_repeat_list(comps).into_iter().next() {
                    m.repeat = r;
                }
            }
        }
        "backdrop-filter" | "-webkit-backdrop-filter" => {
            if let Some(f) = parse_filter(comps) {
                style.backdrop_filter = f;
            }
        }

        // replaced-content fitting (E15-M1).
        "object-fit" => {
            if let Some(f) = object_fit_of(comps) {
                style.object_fit = f;
            }
        }
        "object-position" => {
            if let Some(p) = parse_transform_origin(comps, em_basis, rem, vp) {
                style.object_position = p;
            }
        }
        "image-rendering" => {
            if let Some(r) = image_rendering_of(comps) {
                style.image_rendering = r;
            }
        }

        // CSS counters (E16-M1). `none` clears; otherwise `<name> [<int>]` pairs.
        "counter-reset" => {
            style.counter_reset = parse_counter_list(comps, 0);
        }
        "counter-increment" => {
            style.counter_increment = parse_counter_list(comps, 1);
        }

        // animation (E17-M1). Longhands populate `style.animation` lazily.
        "animation-name" => {
            // `none` leaves the slot untouched; a real name creates/sets it.
            if let [Component::Keyword(k)] = comps {
                if k.eq_ignore_ascii_case("none") {
                    // no-op
                } else {
                    style.animation.get_or_insert_with(Animation::default).name = k.clone();
                }
            } else if let Some(name) = first_ident(comps) {
                if !name.eq_ignore_ascii_case("none") {
                    style.animation.get_or_insert_with(Animation::default).name = name;
                }
            }
        }
        "animation-duration" => {
            if let Some(s) = single_time(comps) {
                style
                    .animation
                    .get_or_insert_with(Animation::default)
                    .duration_s = s;
            }
        }
        "animation-timing-function" => {
            if let Some(e) = parse_easing(comps) {
                style
                    .animation
                    .get_or_insert_with(Animation::default)
                    .timing = e;
            }
        }
        "animation-delay" => {
            if let Some(s) = single_time(comps) {
                style
                    .animation
                    .get_or_insert_with(Animation::default)
                    .delay_s = s;
            }
        }
        "animation-iteration-count" => {
            if let Some(c) = iteration_count_of(comps) {
                style
                    .animation
                    .get_or_insert_with(Animation::default)
                    .iteration_count = c;
            }
        }
        "animation-direction" => {
            if let Some(d) = anim_direction_of(comps) {
                style
                    .animation
                    .get_or_insert_with(Animation::default)
                    .direction = d;
            }
        }
        "animation-fill-mode" => {
            if let Some(f) = anim_fill_mode_of(comps) {
                style
                    .animation
                    .get_or_insert_with(Animation::default)
                    .fill_mode = f;
            }
        }
        "animation" => apply_animation_shorthand(style, comps),

        // transitions (E17-M3). Longhands set one axis across a comma list;
        // index-matching rebuilds `style.transitions` to the longest list.
        "transition-property" => apply_transition_property(style, comps),
        "transition-duration" => apply_transition_times(style, comps, false),
        "transition-delay" => apply_transition_times(style, comps, true),
        "transition-timing-function" => apply_transition_timing(style, comps),
        "transition" => apply_transition_shorthand(style, comps),

        _ => {}
    }
    false
}

/// First `Keyword`/`Color`-free ident in a list (animation-name reads a bare
/// custom-ident; named-color keywords arrive as `Color`, so this never returns
/// them).
fn first_ident(comps: &[Component]) -> Option<String> {
    comps.iter().find_map(|c| match c {
        Component::Keyword(k) => Some(k.clone()),
        _ => None,
    })
}

/// A single `<time>` (s/ms) → seconds. A bare `0` is `0s`.
fn single_time(comps: &[Component]) -> Option<f32> {
    match comps {
        [Component::Dimension { value, unit }] => match unit.as_str() {
            "s" => Some(*value),
            "ms" => Some(*value / 1000.0),
            _ => None,
        },
        [Component::Number(n)] if *n == 0.0 => Some(0.0),
        _ => None,
    }
}

/// `animation-iteration-count`: a `<number>` or `infinite`.
fn iteration_count_of(comps: &[Component]) -> Option<f32> {
    match comps {
        [Component::Number(n)] => Some(n.max(0.0)),
        [Component::Keyword(k)] if k.eq_ignore_ascii_case("infinite") => Some(f32::INFINITY),
        _ => None,
    }
}

fn anim_direction_of(comps: &[Component]) -> Option<AnimDirection> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "normal" => Some(AnimDirection::Normal),
            "reverse" => Some(AnimDirection::Reverse),
            "alternate" => Some(AnimDirection::Alternate),
            "alternate-reverse" => Some(AnimDirection::AlternateReverse),
            _ => None,
        },
        _ => None,
    }
}

fn anim_fill_mode_of(comps: &[Component]) -> Option<AnimFillMode> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "none" => Some(AnimFillMode::None),
            "forwards" => Some(AnimFillMode::Forwards),
            "backwards" => Some(AnimFillMode::Backwards),
            "both" => Some(AnimFillMode::Both),
            _ => None,
        },
        _ => None,
    }
}

/// True if a keyword is a `animation-direction`/`fill-mode`/`infinite` keyword
/// (so the `animation` shorthand can classify it instead of treating it as the
/// name).
fn is_anim_keyword(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "normal"
            | "reverse"
            | "alternate"
            | "alternate-reverse"
            | "none"
            | "forwards"
            | "backwards"
            | "both"
            | "infinite"
            | "running"
            | "paused"
    )
}

/// `animation-timing-function`: a named preset, `cubic-bezier(...)`, or
/// `steps(...)`. Returns `None` for anything unrecognized.
fn parse_easing(comps: &[Component]) -> Option<Easing> {
    match comps {
        [Component::Keyword(k)] => easing_keyword(k),
        [Component::Function { name, raw_args }] => parse_easing_function(name, raw_args),
        _ => None,
    }
}

/// Named easing presets → their cubic-bezier control points (E17-M1).
fn easing_keyword(k: &str) -> Option<Easing> {
    match k.to_ascii_lowercase().as_str() {
        "linear" => Some(Easing::Linear),
        "ease" => Some(Easing::CubicBezier(0.25, 0.1, 0.25, 1.0)),
        "ease-in" => Some(Easing::CubicBezier(0.42, 0.0, 1.0, 1.0)),
        "ease-out" => Some(Easing::CubicBezier(0.0, 0.0, 0.58, 1.0)),
        "ease-in-out" => Some(Easing::CubicBezier(0.42, 0.0, 0.58, 1.0)),
        "step-start" => Some(Easing::Steps(1, JumpTerm::Start)),
        "step-end" => Some(Easing::Steps(1, JumpTerm::End)),
        _ => None,
    }
}

/// Parse a `cubic-bezier(...)`/`steps(...)` easing function from its name and
/// raw arguments.
fn parse_easing_function(name: &str, raw_args: &str) -> Option<Easing> {
    let lower = name.to_ascii_lowercase();
    if lower == "cubic-bezier" {
        let nums: Vec<f32> = raw_args
            .split(',')
            .filter_map(|s| s.trim().parse::<f32>().ok())
            .collect();
        if nums.len() == 4 {
            return Some(Easing::CubicBezier(nums[0], nums[1], nums[2], nums[3]));
        }
        None
    } else if lower == "steps" {
        let mut parts = raw_args.split(',');
        let n: u32 = parts.next()?.trim().parse::<u32>().ok()?;
        let term = match parts.next().map(|s| s.trim().to_ascii_lowercase()) {
            Some(t) if t == "jump-start" || t == "start" => JumpTerm::Start,
            // jump-end / end / any other term → End.
            _ => JumpTerm::End,
        };
        Some(Easing::Steps(n.max(1), term))
    } else {
        None
    }
}

/// Parse the `animation` shorthand (E17-M1): reset to the initial Animation,
/// then classify each component. 1st `<time>` = duration, 2nd = delay; an
/// easing function/keyword = timing; a number/`infinite` = iteration count;
/// direction/fill keywords set those; the remaining ident = name.
fn apply_animation_shorthand(style: &mut ComputedStyle, comps: &[Component]) {
    let mut anim = Animation::default();
    let mut time_seen = 0;
    let mut got_iter = false;
    let mut name: Option<String> = None;

    for c in comps {
        match c {
            Component::Dimension { unit, .. } if unit == "s" || unit == "ms" => {
                if let Some(t) = single_time(std::slice::from_ref(c)) {
                    if time_seen == 0 {
                        anim.duration_s = t;
                    } else if time_seen == 1 {
                        anim.delay_s = t;
                    }
                    time_seen += 1;
                }
            }
            Component::Number(_) if !got_iter => {
                if let Some(n) = iteration_count_of(std::slice::from_ref(c)) {
                    anim.iteration_count = n;
                    got_iter = true;
                }
            }
            Component::Function {
                name: fname,
                raw_args,
            } => {
                if let Some(e) = parse_easing_function(fname, raw_args) {
                    anim.timing = e;
                }
            }
            Component::Keyword(k) => {
                if let Some(e) = easing_keyword(k) {
                    anim.timing = e;
                } else if k.eq_ignore_ascii_case("infinite") {
                    anim.iteration_count = f32::INFINITY;
                    got_iter = true;
                } else if let Some(d) = anim_direction_of(std::slice::from_ref(c)) {
                    anim.direction = d;
                } else if !is_anim_keyword(k) {
                    // `none` (a fill/name keyword) and other anim keywords are
                    // handled above; a remaining custom ident is the name.
                    if name.is_none() {
                        name = Some(k.clone());
                    }
                }
                // `animation-fill-mode` keywords (forwards/backwards/both) and
                // `none` are also valid; classify them onto fill_mode.
                if let Some(f) = anim_fill_mode_of(std::slice::from_ref(c)) {
                    anim.fill_mode = f;
                }
            }
            _ => {}
        }
    }
    if let Some(n) = name {
        anim.name = n;
    }
    style.animation = Some(anim);
}

/// `transition-property`: a comma list of idents / `all` / `none` (E17-M3).
/// `none` clears the list; otherwise each entry becomes a [`Transition`] with
/// default timing fields (later filled by index-matching longhands).
fn apply_transition_property(style: &mut ComputedStyle, comps: &[Component]) {
    if let [Component::Keyword(k)] = comps {
        if k.eq_ignore_ascii_case("none") {
            style.transitions.clear();
            return;
        }
    }
    let mut props: Vec<TransitionProp> = Vec::new();
    for seg in comps.split(|c| matches!(c, Component::Comma)) {
        if let Some(Component::Keyword(k)) = seg.iter().find(|c| matches!(c, Component::Keyword(_)))
        {
            if k.eq_ignore_ascii_case("all") {
                props.push(TransitionProp::All);
            } else if !k.eq_ignore_ascii_case("none") {
                props.push(TransitionProp::Name(k.to_ascii_lowercase()));
            }
        }
    }
    if props.is_empty() {
        return;
    }
    let n = grow_transitions(style, props.len());
    for i in 0..n {
        style.transitions[i].property = props[i % props.len()].clone();
    }
}

/// `transition-duration` (`delay=false`) / `transition-delay` (`delay=true`): a
/// comma list of `<time>` (E17-M3).
fn apply_transition_times(style: &mut ComputedStyle, comps: &[Component], delay: bool) {
    let times: Vec<f32> = comps
        .split(|c| matches!(c, Component::Comma))
        .filter_map(single_time)
        .collect();
    if times.is_empty() {
        return;
    }
    let n = grow_transitions(style, times.len());
    for i in 0..n {
        let v = times[i % times.len()];
        if delay {
            style.transitions[i].delay_s = v;
        } else {
            style.transitions[i].duration_s = v;
        }
    }
}

/// `transition-timing-function`: a comma list of easings (E17-M3).
fn apply_transition_timing(style: &mut ComputedStyle, comps: &[Component]) {
    let easings: Vec<Easing> = comps
        .split(|c| matches!(c, Component::Comma))
        .filter_map(parse_easing)
        .collect();
    if easings.is_empty() {
        return;
    }
    let n = grow_transitions(style, easings.len());
    for i in 0..n {
        style.transitions[i].timing = easings[i % easings.len()];
    }
}

/// Grow `style.transitions` to `max(existing, list_len)` entries, repeating the
/// existing shorter list CSS-style (a previously empty list seeds with
/// defaults). Returns the final length, over which the caller index-matches its
/// own value list with `i % list_len`.
fn grow_transitions(style: &mut ComputedStyle, list_len: usize) -> usize {
    let target = style.transitions.len().max(list_len);
    let old = style.transitions.clone();
    let old_len = old.len();
    while style.transitions.len() < target {
        let next = if old_len == 0 {
            default_transition()
        } else {
            old[style.transitions.len() % old_len].clone()
        };
        style.transitions.push(next);
    }
    target
}

/// The default transition entry (E17-M3): `all 0s ease 0s`.
fn default_transition() -> Transition {
    Transition {
        property: TransitionProp::All,
        duration_s: 0.0,
        timing: Easing::CubicBezier(0.25, 0.1, 0.25, 1.0), // `ease`
        delay_s: 0.0,
    }
}

/// `transition` shorthand (E17-M3): a comma list of single-transition segments.
/// Per segment, the 1st `<time>` is the duration, the 2nd the delay, an easing
/// keyword/function the timing, and the remaining ident the property (`all` →
/// every property). Rebuilds `style.transitions` from scratch.
fn apply_transition_shorthand(style: &mut ComputedStyle, comps: &[Component]) {
    if let [Component::Keyword(k)] = comps {
        if k.eq_ignore_ascii_case("none") {
            style.transitions.clear();
            return;
        }
    }
    let mut out: Vec<Transition> = Vec::new();
    for seg in comps.split(|c| matches!(c, Component::Comma)) {
        let mut tr = default_transition();
        let mut time_seen = 0;
        let mut prop_seen = false;
        for c in seg {
            match c {
                Component::Dimension { unit, .. } if unit == "s" || unit == "ms" => {
                    if let Some(t) = single_time(std::slice::from_ref(c)) {
                        if time_seen == 0 {
                            tr.duration_s = t;
                        } else if time_seen == 1 {
                            tr.delay_s = t;
                        }
                        time_seen += 1;
                    }
                }
                Component::Number(n) if *n == 0.0 => {
                    // a bare `0` is a `<time>`.
                    if time_seen == 0 {
                        tr.duration_s = 0.0;
                    } else if time_seen == 1 {
                        tr.delay_s = 0.0;
                    }
                    time_seen += 1;
                }
                Component::Function { name, raw_args } => {
                    if let Some(e) = parse_easing_function(name, raw_args) {
                        tr.timing = e;
                    }
                }
                Component::Keyword(k) => {
                    if let Some(e) = easing_keyword(k) {
                        tr.timing = e;
                    } else if k.eq_ignore_ascii_case("all") {
                        tr.property = TransitionProp::All;
                        prop_seen = true;
                    } else if k.eq_ignore_ascii_case("none") {
                        // skip
                    } else if !prop_seen {
                        tr.property = TransitionProp::Name(k.to_ascii_lowercase());
                        prop_seen = true;
                    }
                }
                _ => {}
            }
        }
        out.push(tr);
    }
    style.transitions = out;
}

/// Parse a `counter-reset`/`counter-increment` value into `(name, value)` pairs.
/// `none` (or empty) → empty list. Otherwise read `Keyword(name)` optionally
/// followed by a `Number(n)`; a name with no number uses `default` (0 for
/// reset, 1 for increment). Non-keyword/number components end parsing.
fn parse_counter_list(comps: &[Component], default: i32) -> Vec<(String, i32)> {
    if let [Component::Keyword(k)] = comps {
        if k.eq_ignore_ascii_case("none") {
            return Vec::new();
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < comps.len() {
        // A `Keyword(name)` optionally followed by a `Number`; other components
        // are skipped.
        if let Component::Keyword(name) = &comps[i] {
            let value = match comps.get(i + 1) {
                Some(Component::Number(n)) => {
                    i += 1;
                    *n as i32
                }
                _ => default,
            };
            out.push((name.clone(), value));
        }
        i += 1;
    }
    out
}

// --- E13-M2: var() substitution ---

/// True if any component is a `var()` function (the only trigger for the
/// substitution slow path).
fn has_var(comps: &[Component]) -> bool {
    comps
        .iter()
        .any(|c| matches!(c, Component::Function { name, .. } if name == "var"))
}

/// Replace every `var(--name[, fallback])` in `comps` with the custom property's
/// value (or, failing that, its parsed fallback). Returns `None` if any var is
/// unresolved with no fallback, or if substitution recurses too deeply (a
/// reference cycle). Non-var components are cloned through unchanged.
fn substitute_vars(
    comps: &[Component],
    custom: &HashMap<String, Vec<Component>>,
    depth: usize,
) -> Option<Vec<Component>> {
    if depth > 32 {
        return None; // cycle / runaway nesting
    }
    let mut out = Vec::with_capacity(comps.len());
    for c in comps {
        match c {
            Component::Function { name, raw_args } if name == "var" => {
                // Split on the FIRST comma: `--name` (case-preserved) + fallback.
                let (var_name, fallback) = match raw_args.split_once(',') {
                    Some((n, f)) => (n.trim(), Some(f.trim())),
                    None => (raw_args.trim(), None),
                };
                let replacement: Vec<Component> = match custom.get(var_name) {
                    Some(v) => v.clone(),
                    None => match fallback {
                        Some(f) => starfish_css::parse_component_values(f),
                        None => return None, // unresolved, no fallback → invalid
                    },
                };
                // Resolve nested var() inside the replacement.
                let resolved = substitute_vars(&replacement, custom, depth + 1)?;
                out.extend(resolved);
            }
            other => out.push(other.clone()),
        }
    }
    Some(out)
}

// --- value helpers ---

fn as_length(comps: &[Component], em_basis: f32, rem: f32, vp: Viewport) -> Option<Length> {
    if comps.len() != 1 {
        return None;
    }
    // Math-function length (E13-M2 calc, E24-M1 min/max/clamp/round/mod/rem):
    // fires only on a math function; normalizes pure px/% back to Px/Percent so
    // plain lengths stay byte-identical.
    if let Component::Function { name, raw_args } = &comps[0] {
        // env(name[, fallback]) (E24-M3): no real device chrome, so an inset
        // always resolves to its fallback (or 0 when absent).
        if name.eq_ignore_ascii_case("env") {
            return resolve_env_length(raw_args, em_basis, rem, vp);
        }
        if crate::calc::is_math_fn(name) {
            return crate::calc::eval_math_fn(name, raw_args, em_basis, rem, vp);
        }
    }
    match &comps[0] {
        Component::Dimension { value, unit } => match unit.as_str() {
            "px" => Some(Length::Px(*value)),
            "%" => Some(Length::Percent(*value)),
            "em" => Some(Length::Px(*value * em_basis)),
            "rem" => Some(Length::Px(*value * rem)),
            // viewport units (E13-M3) → absolute px against the viewport.
            "vw" => Some(Length::Px(*value / 100.0 * vp.width)),
            "vh" => Some(Length::Px(*value / 100.0 * vp.height)),
            "vmin" => Some(Length::Px(*value / 100.0 * vp.width.min(vp.height))),
            "vmax" => Some(Length::Px(*value / 100.0 * vp.width.max(vp.height))),
            // container query units (E25-M1) → against the nearest query
            // container; cqw/cqi share the inline basis, cqh/cqb the block.
            "cqw" | "cqi" => Some(Length::Px(*value / 100.0 * vp.container_inline)),
            "cqh" | "cqb" => Some(Length::Px(*value / 100.0 * vp.container_block)),
            "cqmin" => Some(Length::Px(
                *value / 100.0 * vp.container_inline.min(vp.container_block),
            )),
            "cqmax" => Some(Length::Px(
                *value / 100.0 * vp.container_inline.max(vp.container_block),
            )),
            _ => None,
        },
        Component::Number(n) if *n == 0.0 => Some(Length::Px(0.0)),
        Component::Keyword(k) if k.eq_ignore_ascii_case("auto") => Some(Length::Auto),
        _ => None,
    }
}

/// Resolve `env(name[, fallback])` to a length (E24-M3). The name is ignored
/// (no device safe-areas); the fallback is parsed as a length, defaulting to
/// `Px(0)` when there's no comma/empty fallback.
fn resolve_env_length(raw_args: &str, em_basis: f32, rem: f32, vp: Viewport) -> Option<Length> {
    match split_first_top_comma(raw_args) {
        Some(fb) if !fb.trim().is_empty() => {
            let comps = starfish_css::parse_component_values(fb.trim());
            as_length(&comps, em_basis, rem, vp)
        }
        _ => Some(Length::Px(0.0)),
    }
}

/// Return the substring after the first top-level (paren-aware) comma, or
/// `None` if there is none.
fn split_first_top_comma(s: &str) -> Option<&str> {
    let mut depth = 0i32;
    for (i, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => return Some(&s[i + 1..]),
            _ => {}
        }
    }
    None
}

/// Length but `auto` is coerced to `Px(0)` (padding can't be auto).
fn as_length_no_auto(comps: &[Component], em_basis: f32, rem: f32, vp: Viewport) -> Option<Length> {
    match as_length(comps, em_basis, rem, vp) {
        Some(Length::Auto) => Some(Length::Px(0.0)),
        other => other,
    }
}

fn set_len(comps: &[Component], em_basis: f32, rem: f32, vp: Viewport, slot: &mut Length) {
    if let Some(l) = as_length(comps, em_basis, rem, vp) {
        *slot = l;
    }
}

/// `aspect-ratio` (E18-M1). Accepts `<number>` (`1.5`), `<w> / <h>` (`16/9`,
/// slash is `Component::Raw("/")`), or `auto`. `auto` (incl. the two-value
/// `auto <ratio>` fallback, whose ratio we ignore for the MVP) ⇒ `None`. A
/// non-positive or malformed ratio leaves the slot unchanged (set_len policy).
fn set_aspect_ratio(comps: &[Component], slot: &mut Option<f32>) {
    // Any `auto` keyword present → ratio fallback not modeled → None.
    if comps
        .iter()
        .any(|c| matches!(c, Component::Keyword(k) if k.eq_ignore_ascii_case("auto")))
    {
        *slot = None;
        return;
    }
    match comps {
        [Component::Number(n)] if *n > 0.0 => *slot = Some(*n),
        [Component::Number(w), Component::Raw(s), Component::Number(h)]
            if s == "/" && *w > 0.0 && *h > 0.0 =>
        {
            *slot = Some(*w / *h)
        }
        _ => {}
    }
}

/// `object-fit` keyword (E15-M1). Unknown keywords ignored (`None`).
fn object_fit_of(comps: &[Component]) -> Option<ObjectFit> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "fill" => Some(ObjectFit::Fill),
            "contain" => Some(ObjectFit::Contain),
            "cover" => Some(ObjectFit::Cover),
            "none" => Some(ObjectFit::None),
            "scale-down" => Some(ObjectFit::ScaleDown),
            _ => None,
        },
        _ => None,
    }
}

/// `image-rendering` keyword (E15-M1). `smooth`/`high-quality` → bilinear;
/// `auto`/`pixelated`/`crisp-edges` → nearest. Unknown keywords ignored.
fn image_rendering_of(comps: &[Component]) -> Option<ImageRendering> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "auto" => Some(ImageRendering::Auto),
            "smooth" | "high-quality" => Some(ImageRendering::Smooth),
            "pixelated" => Some(ImageRendering::Pixelated),
            "crisp-edges" => Some(ImageRendering::CrispEdges),
            _ => None,
        },
        _ => None,
    }
}

/// `box-sizing` keyword (E13-M1). Unknown keywords are ignored (`None`).
fn box_sizing_of(comps: &[Component]) -> Option<BoxSizing> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "content-box" => Some(BoxSizing::ContentBox),
            "border-box" => Some(BoxSizing::BorderBox),
            _ => None,
        },
        _ => None,
    }
}

/// `max-width`/`max-height` value (E13-M1). The `none` keyword maps to the
/// `Length::Auto` "no maximum" sentinel; an explicit `auto` is invalid on max-*
/// and is ignored (leaving the initial Auto). Other values store the parsed
/// length, but a parsed `Auto` is never stored (it would be the wrong sentinel).
fn set_max_len(comps: &[Component], em_basis: f32, rem: f32, vp: Viewport, slot: &mut Length) {
    if let [Component::Keyword(k)] = comps {
        if k.eq_ignore_ascii_case("none") {
            *slot = Length::Auto;
            return;
        }
        if k.eq_ignore_ascii_case("auto") {
            return; // invalid on max-* → ignore
        }
    }
    match as_length(comps, em_basis, rem, vp) {
        Some(Length::Auto) | None => {}
        Some(l) => *slot = l,
    }
}

/// px-only resolvable forms (border widths / font-size). `em` uses `em_basis`.
fn as_px_with(comps: &[Component], em_basis: f32, rem: f32, vp: Viewport) -> Option<f32> {
    if comps.len() != 1 {
        return None;
    }
    // Math functions in a px-only context (E13-M2/E24-M1): a percent part is
    // invalid here, so only a pure-px result (`Length::Px`) is accepted.
    if let Component::Function { name, raw_args } = &comps[0] {
        if crate::calc::is_math_fn(name) {
            return match crate::calc::eval_math_fn(name, raw_args, em_basis, rem, vp)? {
                Length::Px(v) => Some(v),
                _ => None,
            };
        }
    }
    match &comps[0] {
        Component::Dimension { value, unit } => match unit.as_str() {
            "px" => Some(*value),
            "em" => Some(*value * em_basis),
            "rem" => Some(*value * rem),
            // viewport units (E13-M3).
            "vw" => Some(*value / 100.0 * vp.width),
            "vh" => Some(*value / 100.0 * vp.height),
            "vmin" => Some(*value / 100.0 * vp.width.min(vp.height)),
            "vmax" => Some(*value / 100.0 * vp.width.max(vp.height)),
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

fn set_px(comps: &[Component], em_basis: f32, rem: f32, vp: Viewport, slot: &mut f32) {
    if let Some(px) = as_px_with(comps, em_basis, rem, vp) {
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
// --- E16-M2: background layers (image/size/position/repeat) ---

/// Strip a single pair of matching `"`/`'` quotes from a `url(...)` raw arg.
fn strip_quotes(raw: &str) -> String {
    let t = raw.trim();
    let b = t.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        t[1..t.len() - 1].to_string()
    } else {
        t.to_string()
    }
}

/// Split `comps` into top-level comma-separated groups (one per background
/// layer). A `linear-gradient(...)` keeps its inner commas (it's one
/// `Component::Function`), so only `Component::Comma` separators split layers.
fn split_layers(comps: &[Component]) -> Vec<&[Component]> {
    let mut out = Vec::new();
    let mut start = 0;
    for (i, c) in comps.iter().enumerate() {
        if matches!(c, Component::Comma) {
            out.push(&comps[start..i]);
            start = i + 1;
        }
    }
    out.push(&comps[start..]);
    out
}

/// `background-image` → one layer per comma group. A group yields a layer only
/// if it holds a `url(...)` or `linear-gradient(...)`; `none`/unknown groups are
/// skipped. Each layer defaults to size=Auto, position=(0%,0%), repeat=Repeat.
fn parse_bg_image_list(comps: &[Component]) -> Vec<BackgroundLayer> {
    let mut layers = Vec::new();
    for group in split_layers(comps) {
        let mut image = None;
        for c in group {
            if let Component::Function { name, raw_args } = c {
                if name.eq_ignore_ascii_case("url") {
                    image = Some(BgImage::Url(strip_quotes(raw_args)));
                    break;
                }
                if name.eq_ignore_ascii_case("linear-gradient") {
                    if let Some(g) = parse_linear_gradient(raw_args) {
                        image = Some(BgImage::Gradient(g));
                        break;
                    }
                }
                if name.eq_ignore_ascii_case("radial-gradient") {
                    if let Some(g) = parse_radial_gradient(raw_args) {
                        image = Some(BgImage::Radial(g));
                        break;
                    }
                }
                if name.eq_ignore_ascii_case("conic-gradient") {
                    if let Some(g) = parse_conic_gradient(raw_args) {
                        image = Some(BgImage::Conic(g));
                        break;
                    }
                }
            }
        }
        if let Some(image) = image {
            layers.push(BackgroundLayer {
                image,
                size: BgSize::Auto,
                position: (LengthPct::Percent(0.0), LengthPct::Percent(0.0)),
                repeat: BgRepeat::Repeat,
            });
        }
    }
    layers
}

/// `mask-image` (E21-M3) — single-layer reduction of `parse_bg_image_list`. The
/// first usable `url(...)`/`linear-gradient(...)`/`radial-gradient(...)` source
/// becomes a `MaskSpec` (mode Alpha, size Auto, position 0%/0%, repeat Repeat);
/// `none`/unparseable → `None` (clears the mask). Conic masks are not modelled.
fn parse_mask_image(comps: &[Component]) -> Option<MaskSpec> {
    for c in comps {
        if let Component::Function { name, raw_args } = c {
            let image = if name.eq_ignore_ascii_case("url") {
                Some(MaskImage::Url(strip_quotes(raw_args)))
            } else if name.eq_ignore_ascii_case("linear-gradient") {
                parse_linear_gradient(raw_args).map(MaskImage::Gradient)
            } else if name.eq_ignore_ascii_case("radial-gradient") {
                parse_radial_gradient(raw_args).map(MaskImage::Radial)
            } else {
                None
            };
            if let Some(image) = image {
                return Some(MaskSpec {
                    image,
                    mode: MaskMode::Alpha,
                    size: BgSize::Auto,
                    position: (LengthPct::Percent(0.0), LengthPct::Percent(0.0)),
                    repeat: BgRepeat::Repeat,
                });
            }
        }
    }
    None
}

/// One axis token of `background-size`: `auto` / `<percent>` / `<length>`.
fn parse_bg_size_axis(c: &Component, em: f32, rem: f32, vp: Viewport) -> Option<BgSizeAxis> {
    match c {
        Component::Keyword(k) if k.eq_ignore_ascii_case("auto") => Some(BgSizeAxis::Auto),
        Component::Number(n) if *n == 0.0 => Some(BgSizeAxis::Px(0.0)),
        Component::Dimension { value, unit } => {
            match parse_length_pct(&format!("{value}{unit}"), em, rem, vp)? {
                LengthPct::Px(v) => Some(BgSizeAxis::Px(v)),
                LengthPct::Percent(p) => Some(BgSizeAxis::Percent(p)),
            }
        }
        _ => None,
    }
}

/// `background-size` per comma group → one `BgSize`. `cover`/`contain` keywords,
/// else 1-2 axis tokens (`Explicit`; a missing 2nd axis = `Auto`); no usable
/// token → `Auto`.
fn parse_bg_size_list(comps: &[Component], em: f32, rem: f32, vp: Viewport) -> Vec<BgSize> {
    let mut out = Vec::new();
    for group in split_layers(comps) {
        if let [Component::Keyword(k)] = group {
            if k.eq_ignore_ascii_case("cover") {
                out.push(BgSize::Cover);
                continue;
            }
            if k.eq_ignore_ascii_case("contain") {
                out.push(BgSize::Contain);
                continue;
            }
        }
        let mut axes = Vec::new();
        for c in group {
            if let Some(a) = parse_bg_size_axis(c, em, rem, vp) {
                axes.push(a);
                if axes.len() == 2 {
                    break;
                }
            }
        }
        match axes.as_slice() {
            [x] => out.push(BgSize::Explicit(*x, BgSizeAxis::Auto)),
            [x, y] => out.push(BgSize::Explicit(*x, *y)),
            _ => out.push(BgSize::Auto),
        }
    }
    out
}

/// `background-position` per comma group → an `(x, y)` length-pct (reusing the
/// transform-origin parser, which understands keyword + length/percent axes).
fn parse_bg_position_list(
    comps: &[Component],
    em: f32,
    rem: f32,
    vp: Viewport,
) -> Vec<(LengthPct, LengthPct)> {
    split_layers(comps)
        .into_iter()
        .filter_map(|g| parse_transform_origin(g, em, rem, vp))
        .collect()
}

/// `background-repeat` per comma group → one `BgRepeat`.
fn parse_bg_repeat_list(comps: &[Component]) -> Vec<BgRepeat> {
    let mut out = Vec::new();
    for group in split_layers(comps) {
        if let [Component::Keyword(k)] = group {
            let r = match k.to_ascii_lowercase().as_str() {
                "no-repeat" => BgRepeat::NoRepeat,
                "repeat-x" => BgRepeat::RepeatX,
                "repeat-y" => BgRepeat::RepeatY,
                _ => BgRepeat::Repeat,
            };
            out.push(r);
        } else {
            out.push(BgRepeat::Repeat);
        }
    }
    out
}

/// Apply a per-layer value list (cycling with `i % len`) onto the existing
/// layers; a no-op when the list is empty (parse produced nothing usable).
fn apply_bg_sizes(layers: &mut [BackgroundLayer], vals: Vec<BgSize>) {
    if vals.is_empty() {
        return;
    }
    for (i, l) in layers.iter_mut().enumerate() {
        l.size = vals[i % vals.len()];
    }
}

fn apply_bg_positions(layers: &mut [BackgroundLayer], vals: Vec<(LengthPct, LengthPct)>) {
    if vals.is_empty() {
        return;
    }
    for (i, l) in layers.iter_mut().enumerate() {
        l.position = vals[i % vals.len()];
    }
}

fn apply_bg_repeats(layers: &mut [BackgroundLayer], vals: Vec<BgRepeat>) {
    if vals.is_empty() {
        return;
    }
    for (i, l) in layers.iter_mut().enumerate() {
        l.repeat = vals[i % vals.len()];
    }
}

/// `background` shorthand (MVP): the first color → `background_color`; any
/// `url(...)`/`linear-gradient(...)` groups → `background_layers`. This keeps the
/// common forms byte-identical with the old single-`Background` model:
/// `background:red` → color=red, no layers; `background:linear-gradient(...)` →
/// transparent color + one gradient layer. Size/position/repeat in the shorthand
/// are NOT parsed (use the longhands); the shorthand resets layers each time.
fn apply_background_shorthand(
    style: &mut ComputedStyle,
    comps: &[Component],
    _em: f32,
    _rem: f32,
    _vp: Viewport,
) {
    style.background_color = first_color(comps).unwrap_or(TRANSPARENT);
    style.background_layers = parse_bg_image_list(comps);
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

/// Parse `radial-gradient(...)` inner args (E16-M3 MVP). Splits on top-level
/// commas; a leading shape/size/position prefix (e.g. `circle`, `ellipse at
/// center`) that does NOT parse as a color stop is skipped (the prefix is
/// otherwise ignored — ellipse-as-circle, farthest-corner). Needs ≥ 2 stops.
fn parse_radial_gradient(raw_args: &str) -> Option<RadialGradient> {
    let mut stops = Vec::new();
    for seg in split_top_level_commas(raw_args) {
        if let Some(s) = parse_color_stop(&seg) {
            stops.push(s);
        }
        // A non-stop leading segment (shape/size/position) is dropped.
    }
    if stops.len() < 2 {
        return None;
    }
    Some(RadialGradient { stops })
}

/// Parse `conic-gradient(...)` inner args (E16-M3 MVP). An optional leading
/// `from <angle>` segment sets `from_deg` (default 0); an `at ...` part is
/// ignored. Remaining segments are color stops (pos `%`/auto). Needs ≥ 2 stops.
fn parse_conic_gradient(raw_args: &str) -> Option<ConicGradient> {
    let segs = split_top_level_commas(raw_args);
    let mut iter = segs.iter().peekable();
    let mut from_deg = 0.0;

    if let Some(first) = iter.peek() {
        let lower = first.to_ascii_lowercase();
        if lower.starts_with("from") || lower.starts_with("at") {
            // `from <angle> [at <pos>]` / `at <pos>` — consume the prefix segment.
            // Pull the angle following `from` if present (default 0).
            if let Some(rest) = lower.strip_prefix("from") {
                let angle = rest.split("at").next().unwrap_or("").trim();
                if let Some(a) = parse_conic_angle(angle) {
                    from_deg = a;
                }
            }
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
    Some(ConicGradient { from_deg, stops })
}

/// `<angle>` in degrees for `conic-gradient(from ...)`. deg/turn/grad/rad → deg;
/// bare `0` → 0. `None` if not an angle.
fn parse_conic_angle(s: &str) -> Option<f32> {
    let t = s.trim().to_ascii_lowercase();
    let pick = |suf: &str| {
        t.strip_suffix(suf)
            .and_then(|n| n.trim().parse::<f32>().ok())
    };
    if let Some(v) = pick("grad") {
        return Some(v * 0.9);
    }
    if let Some(v) = pick("turn") {
        return Some(v * 360.0);
    }
    if let Some(v) = pick("rad") {
        return Some(v.to_degrees());
    }
    if let Some(v) = pick("deg") {
        return Some(v);
    }
    if t == "0" {
        return Some(0.0);
    }
    None
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
fn parse_transform(
    comps: &[Component],
    em: f32,
    rem: f32,
    vp: Viewport,
) -> Option<Vec<TransformFn>> {
    if let [Component::Keyword(k)] = comps {
        if k.eq_ignore_ascii_case("none") {
            return Some(Vec::new());
        }
    }
    let mut out = Vec::new();
    for c in comps {
        if let Component::Function { name, raw_args } = c {
            if let Some(f) = parse_transform_fn(name, raw_args, em, rem, vp) {
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
fn parse_transform_fn(
    name: &str,
    raw: &str,
    em: f32,
    rem: f32,
    vp: Viewport,
) -> Option<TransformFn> {
    let args = split_top_level_commas(raw);
    match name.to_ascii_lowercase().as_str() {
        "translate" => {
            let x = parse_length_pct(args.first()?, em, rem, vp)?;
            let y = match args.get(1) {
                Some(s) => parse_length_pct(s, em, rem, vp)?,
                None => LengthPct::Px(0.0),
            };
            Some(TransformFn::Translate(x, y))
        }
        "translatex" => Some(TransformFn::Translate(
            parse_length_pct(args.first()?, em, rem, vp)?,
            LengthPct::Px(0.0),
        )),
        "translatey" => Some(TransformFn::Translate(
            LengthPct::Px(0.0),
            parse_length_pct(args.first()?, em, rem, vp)?,
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

// --- blend modes (E21-M2) ---

/// Map a `mix-blend-mode`/`background-blend-mode` keyword to a [`BlendMode`],
/// or `None` for an unknown keyword.
fn blend_mode_kw(k: &str) -> Option<BlendMode> {
    Some(match k.to_ascii_lowercase().as_str() {
        "normal" => BlendMode::Normal,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "color-dodge" => BlendMode::ColorDodge,
        "color-burn" => BlendMode::ColorBurn,
        "hard-light" => BlendMode::HardLight,
        "soft-light" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        _ => return None,
    })
}

// --- filter (E21-M1) ---

/// `none` → empty list; else parse each `Function` left-to-right. An
/// unrecognized / malformed function is skipped (lenient). Returns `None` (leave
/// unchanged) only when nothing parseable is present and it isn't `none`.
/// Mirrors `parse_transform`.
fn parse_filter(comps: &[Component]) -> Option<Vec<FilterFn>> {
    if let [Component::Keyword(k)] = comps {
        if k.eq_ignore_ascii_case("none") {
            return Some(Vec::new());
        }
    }
    let mut out = Vec::new();
    for c in comps {
        if let Component::Function { name, raw_args } = c {
            if let Some(f) = parse_filter_fn(name, raw_args) {
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

/// One `name(raw_args)` → a `FilterFn`. Args split on top-level commas.
fn parse_filter_fn(name: &str, raw: &str) -> Option<FilterFn> {
    let args = split_top_level_commas(raw);
    let first = args.first().map(|s| s.trim());
    match name.to_ascii_lowercase().as_str() {
        "blur" => {
            // Only px lengths (and bare 0). Default 0 if absent.
            let px = match first {
                None | Some("") => 0.0,
                Some(s) => match s.strip_suffix("px") {
                    Some(n) => n.trim().parse::<f32>().ok()?,
                    None if s == "0" => 0.0,
                    None => return None,
                },
            };
            Some(FilterFn::Blur(px.max(0.0)))
        }
        "brightness" => Some(FilterFn::Brightness(parse_amount(first).unwrap_or(1.0))),
        "contrast" => Some(FilterFn::Contrast(parse_amount(first).unwrap_or(1.0))),
        "saturate" => Some(FilterFn::Saturate(parse_amount(first).unwrap_or(1.0))),
        "grayscale" => Some(FilterFn::Grayscale(
            parse_amount(first).unwrap_or(1.0).clamp(0.0, 1.0),
        )),
        "sepia" => Some(FilterFn::Sepia(
            parse_amount(first).unwrap_or(1.0).clamp(0.0, 1.0),
        )),
        "invert" => Some(FilterFn::Invert(
            parse_amount(first).unwrap_or(1.0).clamp(0.0, 1.0),
        )),
        "opacity" => Some(FilterFn::Opacity(
            parse_amount(first).unwrap_or(1.0).clamp(0.0, 1.0),
        )),
        "hue-rotate" => Some(FilterFn::HueRotate(
            first.and_then(parse_angle_rad).unwrap_or(0.0),
        )),
        "drop-shadow" => parse_drop_shadow(raw),
        _ => None,
    }
}

/// A filter `<number-percentage>`: `"50%"` → 0.5, `"1.2"` → 1.2. `None` keeps the
/// caller's default.
fn parse_amount(s: Option<&str>) -> Option<f32> {
    let s = s?.trim();
    if let Some(p) = s.strip_suffix('%') {
        return p.trim().parse::<f32>().ok().map(|v| v / 100.0);
    }
    s.parse::<f32>().ok()
}

/// `drop-shadow(<dx> <dy> <blur>? <color>?)`. Collects 2–3 px lengths and the
/// first color (default black). Mirrors `parse_box_shadow` but on the re-parsed
/// function args (so colors resolve). `None` if fewer than 2 lengths.
fn parse_drop_shadow(raw: &str) -> Option<FilterFn> {
    let comps = parse_component_values(raw);
    let mut lengths: Vec<f32> = Vec::new();
    let mut color = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    }; // default ≈ currentColor → black
    for c in &comps {
        match c {
            Component::Dimension { value, unit } if unit == "px" => lengths.push(*value),
            Component::Number(n) if *n == 0.0 => lengths.push(0.0),
            Component::Color(rgba) => color = *rgba,
            _ => {}
        }
    }
    match lengths.as_slice() {
        [dx, dy] => Some(FilterFn::DropShadow {
            dx: *dx,
            dy: *dy,
            blur: 0.0,
            color,
        }),
        [dx, dy, blur, ..] => Some(FilterFn::DropShadow {
            dx: *dx,
            dy: *dy,
            blur: blur.max(0.0),
            color,
        }),
        _ => None,
    }
}

/// `<length-percentage>`: "20px"|"%"|"em"|"rem"; a bare "0" → Px(0).
fn parse_length_pct(s: &str, em: f32, rem: f32, vp: Viewport) -> Option<LengthPct> {
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
    if let Some(p) = t.strip_suffix("vmin") {
        return p
            .trim()
            .parse()
            .ok()
            .map(|v: f32| LengthPct::Px(v / 100.0 * vp.width.min(vp.height)));
    }
    if let Some(p) = t.strip_suffix("vmax") {
        return p
            .trim()
            .parse()
            .ok()
            .map(|v: f32| LengthPct::Px(v / 100.0 * vp.width.max(vp.height)));
    }
    if let Some(p) = t.strip_suffix("vw") {
        return p
            .trim()
            .parse()
            .ok()
            .map(|v: f32| LengthPct::Px(v / 100.0 * vp.width));
    }
    if let Some(p) = t.strip_suffix("vh") {
        return p
            .trim()
            .parse()
            .ok()
            .map(|v: f32| LengthPct::Px(v / 100.0 * vp.height));
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
    let pick = |suf: &str| {
        t.strip_suffix(suf)
            .and_then(|n| n.trim().parse::<f32>().ok())
    };
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
    vp: Viewport,
) -> Option<(LengthPct, LengthPct)> {
    let mut xs: Vec<LengthPct> = Vec::new();
    for c in comps {
        match c {
            Component::Dimension { value, unit } => match unit.as_str() {
                "px" => xs.push(LengthPct::Px(*value)),
                "%" => xs.push(LengthPct::Percent(*value)),
                "em" => xs.push(LengthPct::Px(*value * em)),
                "rem" => xs.push(LengthPct::Px(*value * rem)),
                "vw" => xs.push(LengthPct::Px(*value / 100.0 * vp.width)),
                "vh" => xs.push(LengthPct::Px(*value / 100.0 * vp.height)),
                "vmin" => xs.push(LengthPct::Px(*value / 100.0 * vp.width.min(vp.height))),
                "vmax" => xs.push(LengthPct::Px(*value / 100.0 * vp.width.max(vp.height))),
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
fn border_radius_shorthand(
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) -> Option<[f32; 4]> {
    let mut vals = Vec::with_capacity(4);
    for c in comps {
        match c {
            // a `/` arrives as a raw token; stop reading the horizontal radii.
            Component::Raw(t) if t == "/" => break,
            _ => {
                if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem, vp) {
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
fn parse_box_shadow(
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) -> Option<BoxShadow> {
    let mut lengths = Vec::new();
    let mut color = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    }; // default ≈ currentColor → black
    for c in comps {
        match c {
            Component::Dimension { .. } | Component::Number(_) => {
                if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem, vp) {
                    lengths.push(px);
                }
            }
            Component::Color(rgba) => color = *rgba,
            Component::Keyword(k) if k.eq_ignore_ascii_case("none") => return None,
            _ => {}
        }
    }
    match lengths.as_slice() {
        [x, y] => Some(BoxShadow {
            offset_x: *x,
            offset_y: *y,
            blur: 0.0,
            spread: 0.0,
            color,
        }),
        [x, y, b] => Some(BoxShadow {
            offset_x: *x,
            offset_y: *y,
            blur: *b,
            spread: 0.0,
            color,
        }),
        [x, y, b, s] => Some(BoxShadow {
            offset_x: *x,
            offset_y: *y,
            blur: *b,
            spread: *s,
            color,
        }),
        _ => None,
    }
}

/// `text-shadow: <offset-x> <offset-y> <blur>? || <color>` — single layer, no
/// spread (E16-M3). `none` → None. If comma-separated, only the first layer is
/// kept. Default color = black (≈ currentColor).
fn parse_text_shadow(
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) -> Option<TextShadow> {
    if let [Component::Keyword(k)] = comps {
        if k.eq_ignore_ascii_case("none") {
            return None;
        }
    }
    // Take the first comma-separated layer.
    let layer = split_layers(comps).into_iter().next().unwrap_or(comps);
    let mut lengths = Vec::new();
    let mut color = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
    let mut color_set = false;
    for c in layer {
        match c {
            Component::Dimension { .. } | Component::Number(_) => {
                if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem, vp) {
                    if lengths.len() < 3 {
                        lengths.push(px);
                    }
                }
            }
            Component::Color(rgba) if !color_set => {
                color = *rgba;
                color_set = true;
            }
            _ => {}
        }
    }
    match lengths.as_slice() {
        [x, y] => Some(TextShadow {
            offset_x: *x,
            offset_y: *y,
            blur: 0.0,
            color,
        }),
        [x, y, b] => Some(TextShadow {
            offset_x: *x,
            offset_y: *y,
            blur: *b,
            color,
        }),
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

fn set_len_no_auto(comps: &[Component], em_basis: f32, rem: f32, vp: Viewport, slot: &mut Length) {
    if let Some(l) = as_length_no_auto(comps, em_basis, rem, vp) {
        *slot = l;
    }
}

/// `flex` shorthand. Sets flex_grow / flex_shrink / flex_basis together.
fn apply_flex_shorthand(
    style: &mut ComputedStyle,
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) {
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
                if let Some(l) = as_length(std::slice::from_ref(c), em_basis, rem, vp) {
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
fn apply_gap_shorthand(
    style: &mut ComputedStyle,
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) {
    let mut lens = Vec::with_capacity(2);
    for c in comps {
        if let Some(l) = as_length_no_auto(std::slice::from_ref(c), em_basis, rem, vp) {
            lens.push(l);
        }
    }
    match lens.as_slice() {
        [g] => {
            style.row_gap = g.clone();
            style.column_gap = g.clone();
        }
        [r, c, ..] => {
            style.row_gap = r.clone();
            style.column_gap = c.clone();
        }
        [] => {}
    }
}

/// `border-spacing: <length> [<length>]`. One value → both axes; two → (h, v).
/// `auto`/percent are invalid here → ignored. (E7-M3)
fn apply_border_spacing(
    style: &mut ComputedStyle,
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) {
    let mut vals = Vec::with_capacity(2);
    for c in comps {
        if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem, vp) {
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
            "sticky" => Some(Position::Sticky),
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

/// `overflow` value → an `Overflow` from the first keyword. `scroll`/`auto` map
/// to `Visible`: we don't render scrollbars, and clipping scrollable content
/// would hide what a scrollbar would otherwise reveal (E13-M4). Unknown → None
/// (leaves the property unchanged).
fn overflow_of(comps: &[Component]) -> Option<Overflow> {
    for c in comps {
        if let Component::Keyword(k) = c {
            return match k.to_ascii_lowercase().as_str() {
                "visible" => Some(Overflow::Visible),
                "hidden" => Some(Overflow::Hidden),
                "clip" => Some(Overflow::Clip),
                "scroll" | "auto" => Some(Overflow::Visible),
                _ => None,
            };
        }
    }
    None
}

fn text_overflow_of(comps: &[Component]) -> Option<TextOverflow> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "clip" => Some(TextOverflow::Clip),
            "ellipsis" => Some(TextOverflow::Ellipsis),
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
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        // solid + groove/ridge/inset/outset all fold to Solid.
        _ => BorderStyle::Solid,
    }
}

fn is_style_keyword(k: &str) -> bool {
    matches!(
        k.to_ascii_lowercase().as_str(),
        "none"
            | "hidden"
            | "solid"
            | "dashed"
            | "dotted"
            | "double"
            | "groove"
            | "ridge"
            | "inset"
            | "outset"
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
        [Component::Keyword(k), ..] if k.eq_ignore_ascii_case("oblique") => {
            Some(FontStyle::Oblique)
        }
        _ => None,
    }
}

fn line_height_of(
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) -> Option<LineHeight> {
    match comps {
        [Component::Number(n)] => Some(LineHeight::Number(*n)),
        [Component::Keyword(k)] if k.eq_ignore_ascii_case("normal") => Some(LineHeight::Normal),
        [Component::Dimension { value, unit }] => match unit.as_str() {
            "px" => Some(LineHeight::Px(*value)),
            "em" => Some(LineHeight::Px(*value * em_basis)),
            "rem" => Some(LineHeight::Px(*value * rem)),
            "%" => Some(LineHeight::Number(*value / 100.0)),
            "vw" => Some(LineHeight::Px(*value / 100.0 * vp.width)),
            "vh" => Some(LineHeight::Px(*value / 100.0 * vp.height)),
            "vmin" => Some(LineHeight::Px(*value / 100.0 * vp.width.min(vp.height))),
            "vmax" => Some(LineHeight::Px(*value / 100.0 * vp.width.max(vp.height))),
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

/// `text-indent: <length>|<percentage>`. Negatives allowed; `0` → `Px(0)`.
/// `inter-character`/`distribute` are not valid here (handled by text-justify).
fn text_indent_of(comps: &[Component], em: f32, rem: f32, vp: Viewport) -> Option<LengthPct> {
    match comps {
        [Component::Number(n)] if *n == 0.0 => Some(LengthPct::Px(0.0)),
        [Component::Dimension { value, unit }] => match unit.as_str() {
            "px" => Some(LengthPct::Px(*value)),
            "%" => Some(LengthPct::Percent(*value)),
            "em" => Some(LengthPct::Px(*value * em)),
            "rem" => Some(LengthPct::Px(*value * rem)),
            "vw" => Some(LengthPct::Px(*value / 100.0 * vp.width)),
            "vh" => Some(LengthPct::Px(*value / 100.0 * vp.height)),
            "vmin" => Some(LengthPct::Px(*value / 100.0 * vp.width.min(vp.height))),
            "vmax" => Some(LengthPct::Px(*value / 100.0 * vp.width.max(vp.height))),
            _ => None,
        },
        _ => None,
    }
}

/// `text-justify: auto|inter-word|none`. `inter-character`/`distribute` fold to
/// `None`-ignored (left unchanged) per the design (E22-M2).
fn text_justify_of(comps: &[Component]) -> Option<TextJustify> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "auto" => Some(TextJustify::Auto),
            "inter-word" => Some(TextJustify::InterWord),
            "none" => Some(TextJustify::None),
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

fn writing_mode_of(comps: &[Component]) -> Option<WritingMode> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "horizontal-tb" => Some(WritingMode::HorizontalTb),
            "vertical-rl" => Some(WritingMode::VerticalRl),
            "vertical-lr" => Some(WritingMode::VerticalLr),
            // sideways-rl / sideways-lr are not modeled → dropped.
            _ => None,
        },
        _ => None,
    }
}

/// `container-type` keyword (E25-M1). `None` for an unrecognized value.
fn container_type_of(comps: &[Component]) -> Option<ContainerType> {
    comps.iter().find_map(|c| match c {
        Component::Keyword(k) => match k.to_ascii_lowercase().as_str() {
            "normal" => Some(ContainerType::Normal),
            "inline-size" => Some(ContainerType::InlineSize),
            "size" => Some(ContainerType::Size),
            _ => None,
        },
        _ => None,
    })
}

/// `container-name` (E25-M1): the first name ident, or `None` for `none`/empty.
fn container_name_of(comps: &[Component]) -> Option<String> {
    comps.iter().find_map(|c| match c {
        Component::Keyword(k) if !k.eq_ignore_ascii_case("none") => Some(k.to_ascii_lowercase()),
        _ => None,
    })
}

/// `container: <name> [ / <type> ]` shorthand (E25-M1).
fn apply_container_shorthand(style: &mut ComputedStyle, comps: &[Component]) {
    let slash = comps
        .iter()
        .position(|c| matches!(c, Component::Raw(s) if s == "/"));
    let (name_part, type_part) = match slash {
        Some(i) => (&comps[..i], &comps[i + 1..]),
        None => (comps, &[][..]),
    };
    style.container_name = container_name_of(name_part);
    if let Some(t) = container_type_of(type_part) {
        style.container_type = t;
    }
}

fn text_orientation_of(comps: &[Component]) -> Option<TextOrientation> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "mixed" => Some(TextOrientation::Mixed),
            "upright" => Some(TextOrientation::Upright),
            "sideways" => Some(TextOrientation::Sideways),
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
fn spacing_of(comps: &[Component], em: f32, rem: f32, vp: Viewport) -> Option<f32> {
    match comps {
        [Component::Keyword(k)] if k.eq_ignore_ascii_case("normal") => Some(0.0),
        _ => as_px_with(comps, em, rem, vp),
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
            "break-spaces" => Some(WhiteSpace::BreakSpaces),
            _ => None,
        },
        _ => None,
    }
}

fn word_break_of(comps: &[Component]) -> Option<WordBreak> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "normal" => Some(WordBreak::Normal),
            "break-all" => Some(WordBreak::BreakAll),
            "keep-all" => Some(WordBreak::KeepAll),
            _ => None,
        },
        _ => None,
    }
}

fn overflow_wrap_of(comps: &[Component]) -> Option<OverflowWrap> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "normal" => Some(OverflowWrap::Normal),
            "break-word" => Some(OverflowWrap::BreakWord),
            "anywhere" => Some(OverflowWrap::Anywhere),
            _ => None,
        },
        _ => None,
    }
}

fn tab_size_of(comps: &[Component], em_basis: f32, rem: f32, vp: Viewport) -> Option<TabSize> {
    match comps {
        [Component::Number(n)] if *n >= 0.0 => Some(TabSize::Number(*n)),
        _ => as_px_with(comps, em_basis, rem, vp)
            .filter(|p| *p >= 0.0)
            .map(TabSize::Px),
    }
}

fn hyphens_of(comps: &[Component]) -> Option<Hyphens> {
    match comps {
        [Component::Keyword(k)] => match k.to_ascii_lowercase().as_str() {
            "none" => Some(Hyphens::None),
            "manual" => Some(Hyphens::Manual),
            "auto" => Some(Hyphens::Auto),
            _ => None,
        },
        _ => None,
    }
}

/// `-webkit-line-clamp` / `line-clamp`. `none` or any invalid value → `None`;
/// a positive integer `n` → `Some(n)`. The `line-clamp` shorthand may carry
/// extra components — we take the first positive integer.
fn line_clamp_of(comps: &[Component]) -> Option<u32> {
    for c in comps {
        if let Component::Number(n) = c {
            if *n >= 1.0 && n.fract() == 0.0 {
                return Some(*n as u32);
            }
        }
    }
    None
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
    vp: Viewport,
    no_auto: bool,
) -> Option<[Length; 4]> {
    let mut vals = Vec::with_capacity(4);
    for c in comps {
        let slice = std::slice::from_ref(c);
        let l = if no_auto {
            as_length_no_auto(slice, em_basis, rem, vp)
        } else {
            as_length(slice, em_basis, rem, vp)
        };
        vals.push(l?);
    }
    expand4(&vals)
}

fn shorthand_px(comps: &[Component], em_basis: f32, rem: f32, vp: Viewport) -> Option<[f32; 4]> {
    let mut vals = Vec::with_capacity(4);
    for c in comps {
        let slice = std::slice::from_ref(c);
        let px = as_px_with(slice, em_basis, rem, vp).or_else(|| border_width_keyword(slice))?;
        vals.push(px);
    }
    expand4(&vals)
}

/// CSS 1–4 value expansion to [top, right, bottom, left].
fn expand4<T: Clone>(vals: &[T]) -> Option<[T; 4]> {
    match vals {
        [a] => Some([a.clone(), a.clone(), a.clone(), a.clone()]),
        [v, h] => Some([v.clone(), h.clone(), v.clone(), h.clone()]),
        [t, h, b] => Some([t.clone(), h.clone(), b.clone(), h.clone()]),
        [t, r, b, l] => Some([t.clone(), r.clone(), b.clone(), l.clone()]),
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
    vp: Viewport,
) -> bool {
    let mut color_set = false;
    for c in comps {
        match c {
            Component::Dimension { .. } | Component::Number(_) => {
                if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem, vp) {
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

/// `outline: <width> || <style> || <color>` in any order (E16-M3). Mirrors
/// `apply_border_shorthand` but writes into `style.outline`. `outline-offset` is
/// NOT part of this shorthand (it has its own longhand).
fn apply_outline_shorthand(
    style: &mut ComputedStyle,
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) {
    for c in comps {
        match c {
            Component::Dimension { .. } | Component::Number(_) => {
                if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem, vp) {
                    style.outline.width = px;
                }
            }
            Component::Color(rgba) => style.outline.color = *rgba,
            Component::Keyword(k) if k.eq_ignore_ascii_case("transparent") => {
                style.outline.color = TRANSPARENT;
            }
            // `auto` outline-style → Solid.
            Component::Keyword(k) if k.eq_ignore_ascii_case("auto") => {
                style.outline.style = BorderStyle::Solid;
            }
            Component::Keyword(k) if is_style_keyword(k) => {
                style.outline.style = style_keyword(k);
            }
            Component::Keyword(_) => {
                if let Some(px) = border_width_keyword(std::slice::from_ref(c)) {
                    style.outline.width = px;
                }
            }
            _ => {}
        }
    }
}

/// `columns` shorthand (E18-M2): `<integer>` → column-count, `<length>` →
/// column-width, `auto` resets the matching slot it would otherwise fill.
fn apply_columns_shorthand(
    style: &mut ComputedStyle,
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) {
    let mut count_set = false;
    let mut width_set = false;
    for c in comps {
        match c {
            // Bare integer → column-count.
            Component::Number(n) if *n >= 1.0 && n.fract() == 0.0 => {
                style.column_count = Some(*n as u32);
                count_set = true;
            }
            Component::Dimension { .. } => {
                if let Some(l) = as_length(std::slice::from_ref(c), em_basis, rem, vp) {
                    style.column_width = Some(l);
                    width_set = true;
                }
            }
            Component::Keyword(k) if k.eq_ignore_ascii_case("auto") => {
                // `auto` resets whichever slot has not yet been filled.
                if !count_set {
                    style.column_count = None;
                }
                if !width_set {
                    style.column_width = None;
                }
            }
            _ => {}
        }
    }
}

/// `column-rule` shorthand (E18-M2): width || style || color in any order.
/// Mirrors `apply_outline_shorthand`, writing into the `column_rule_*` slots.
fn apply_column_rule_shorthand(
    style: &mut ComputedStyle,
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) {
    for c in comps {
        match c {
            Component::Dimension { .. } | Component::Number(_) => {
                if let Some(px) = as_px_with(std::slice::from_ref(c), em_basis, rem, vp) {
                    style.column_rule_width = px;
                }
            }
            Component::Color(rgba) => style.column_rule_color = *rgba,
            Component::Keyword(k) if k.eq_ignore_ascii_case("transparent") => {
                style.column_rule_color = TRANSPARENT;
            }
            Component::Keyword(k) if is_style_keyword(k) => {
                style.column_rule_style = style_keyword(k);
            }
            Component::Keyword(_) => {
                if let Some(px) = border_width_keyword(std::slice::from_ref(c)) {
                    style.column_rule_width = px;
                }
            }
            _ => {}
        }
    }
}

// --- E5-M1: grid track lists + line placement ---

/// Parse a `grid-template-columns`/`-rows` track list. `None` (declaration
/// ignored) on an empty / `none` / unsupported (`auto-fill`/`minmax`) value.
fn track_list_of(
    comps: &[Component],
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) -> Option<Vec<TrackSize>> {
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
                expand_repeat(raw_args, em_basis, rem, vp, &mut out)?;
            }
            _ => {
                let t = track_size_of_component(c, em_basis, rem, vp)?; // unknown → whole list fails
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
fn track_size_of_component(
    c: &Component,
    em_basis: f32,
    rem: f32,
    vp: Viewport,
) -> Option<TrackSize> {
    match c {
        Component::Dimension { value, unit } => match unit.as_str() {
            "px" => Some(TrackSize::Px(*value)),
            "em" => Some(TrackSize::Px(*value * em_basis)),
            "rem" => Some(TrackSize::Px(*value * rem)),
            "%" => Some(TrackSize::Percent(*value)),
            "fr" => Some(TrackSize::Fr(value.max(0.0))),
            "vw" => Some(TrackSize::Px(*value / 100.0 * vp.width)),
            "vh" => Some(TrackSize::Px(*value / 100.0 * vp.height)),
            "vmin" => Some(TrackSize::Px(*value / 100.0 * vp.width.min(vp.height))),
            "vmax" => Some(TrackSize::Px(*value / 100.0 * vp.width.max(vp.height))),
            _ => None,
        },
        // `0` (a bare Number) is a valid `0px` track.
        Component::Number(n) if *n == 0.0 => Some(TrackSize::Px(0.0)),
        Component::Keyword(k) if k.eq_ignore_ascii_case("auto") => Some(TrackSize::Auto),
        _ => None,
    }
}

/// One raw track token (`100px`, `1fr`, `auto`, `50%`, `0`) → `TrackSize`.
fn track_size_of_token(tok: &str, em_basis: f32, rem: f32, vp: Viewport) -> Option<TrackSize> {
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
        return n
            .trim()
            .parse::<f32>()
            .ok()
            .map(|v| TrackSize::Fr(v.max(0.0)));
    }
    if let Some(n) = t.strip_suffix('%') {
        return n.trim().parse::<f32>().ok().map(TrackSize::Percent);
    }
    if let Some(n) = t.strip_suffix("rem") {
        return n.trim().parse::<f32>().ok().map(|v| TrackSize::Px(v * rem));
    }
    // viewport units before the shorter "em" suffix wouldn't collide, but check
    // them explicitly (vw/vh/vmin/vmax) before falling through to "em".
    if let Some(n) = t.strip_suffix("vmin") {
        return n
            .trim()
            .parse::<f32>()
            .ok()
            .map(|v| TrackSize::Px(v / 100.0 * vp.width.min(vp.height)));
    }
    if let Some(n) = t.strip_suffix("vmax") {
        return n
            .trim()
            .parse::<f32>()
            .ok()
            .map(|v| TrackSize::Px(v / 100.0 * vp.width.max(vp.height)));
    }
    if let Some(n) = t.strip_suffix("vw") {
        return n
            .trim()
            .parse::<f32>()
            .ok()
            .map(|v| TrackSize::Px(v / 100.0 * vp.width));
    }
    if let Some(n) = t.strip_suffix("vh") {
        return n
            .trim()
            .parse::<f32>()
            .ok()
            .map(|v| TrackSize::Px(v / 100.0 * vp.height));
    }
    if let Some(n) = t.strip_suffix("em") {
        return n
            .trim()
            .parse::<f32>()
            .ok()
            .map(|v| TrackSize::Px(v * em_basis));
    }
    None
}

/// Expand `repeat(<int>, <tracklist>)`. `raw_args` is the verbatim inner text,
/// e.g. `"3, 1fr"` or `"2, 100px 1fr"`. We split on the first comma (the count),
/// then split the rest on whitespace and parse each token. `auto-fill`/`auto-fit`
/// (non-integer count) → `None` (drop the declaration).
fn expand_repeat(
    raw_args: &str,
    em_basis: f32,
    rem: f32,
    vp: Viewport,
    out: &mut Vec<TrackSize>,
) -> Option<()> {
    let (count_str, rest) = raw_args.split_once(',')?;
    let n: usize = count_str.trim().parse().ok()?; // non-integer (auto-fill) → None
    if n == 0 || n > 1000 {
        return None; // guard absurd counts
    }
    let mut one: Vec<TrackSize> = Vec::new();
    for tok in rest.split_ascii_whitespace() {
        one.push(track_size_of_token(tok, em_basis, rem, vp)?);
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
    let has_slash = comps
        .iter()
        .any(|c| matches!(c, Component::Raw(s) if s == "/"));
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
