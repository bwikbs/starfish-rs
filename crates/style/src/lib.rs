//! starfish-style — style resolution (M3).
//!
//! Given a [`Document`] and parsed author [`Stylesheet`]s, produce a
//! [`StyledTree`]: one typed [`ComputedStyle`] per element, after selector
//! matching, the cascade, and inheritance. See `docs/design/M3-style.md`.

mod calc;
mod cascade;
mod computed;
mod counters;
mod interpolate;
mod matching;
mod media;
mod properties;
mod ua;

use std::collections::HashMap;

use starfish_css::{Declaration, KeyframesRule, Rule, Stylesheet};
use starfish_dom::Document;

pub use computed::{
    AlignItems, AlignSelf, AnimDirection, AnimFillMode, Animation, BackgroundLayer, BgImage,
    BgRepeat, BgSize, BgSizeAxis, BlendMode, BorderCollapse, BorderStyle, BoxShadow, BoxSizing,
    Clear, ComputedStyle, ConicGradient, Content, Direction, Display, Easing, FilterFn,
    FlexDirection, FlexWrap, Float, FontStyle, FontWeight, GradientStop, GridLine, GridPlacement,
    ImageRendering, JumpTerm, JustifyContent, Length, LengthPct, LineHeight, LinearGradient,
    ListStylePosition, ListStyleType, MaskImage, MaskMode, MaskSpec, ObjectFit, Outline, Overflow,
    OverflowWrap, Position, RadialGradient, TabSize, TextAlign, TextDecorationLine, TextJustify,
    TextOrientation, TextOverflow, TextShadow, TextTransform, TrackSize, TransformFn, Transition,
    TransitionProp, UnicodeBidi, WhiteSpace, WordBreak, WritingMode,
};
pub use matching::matches;
pub use media::media_matches;
pub use starfish_css::{PseudoElement, Rgba};
pub use starfish_dom::NodeId;

use cascade::{cascade, cascade_pseudo, CascadeCache, Origin};
use properties::EmContext;

/// The render viewport, in CSS px (E13-M3). Threaded into the cascade so
/// `@media` queries and `vw`/`vh`/`vmin`/`vmax` units can resolve against it.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
}

impl Viewport {
    /// Build a viewport from a width, assuming a deterministic 4:3 aspect ratio
    /// (height = width × 0.75; 800 → 600). Layout sizes the page off the width;
    /// this gives `vh`/orientation a stable height without a real layout pass.
    pub fn from_width(width: f32) -> Viewport {
        Viewport {
            width,
            height: width * 0.75,
        }
    }
}

/// Side table mapping each styled element to its computed style.
#[derive(Debug, Default)]
pub struct StyledTree {
    styles: HashMap<NodeId, ComputedStyle>,
    /// Generated `::before` pseudo: element → (pseudo style, content text) (E7-M2).
    before: HashMap<NodeId, (ComputedStyle, String)>,
    /// Generated `::after` pseudo.
    after: HashMap<NodeId, (ComputedStyle, String)>,
}

impl StyledTree {
    /// Computed style for a node. Panics if `id` was not styled (a bug — every
    /// element is inserted during the walk).
    pub fn computed(&self, id: NodeId) -> &ComputedStyle {
        &self.styles[&id]
    }

    /// Non-panicking lookup (None for Document/Doctype/Comment/Text nodes).
    pub fn get(&self, id: NodeId) -> Option<&ComputedStyle> {
        self.styles.get(&id)
    }

    /// The computed style + content text for `id`'s `::before`/`::after` pseudo,
    /// if one was generated (E7-M2).
    pub fn pseudo(&self, id: NodeId, side: PseudoElement) -> Option<&(ComputedStyle, String)> {
        match side {
            PseudoElement::Before => self.before.get(&id),
            PseudoElement::After => self.after.get(&id),
        }
    }

    /// The pseudo style alone (for paint/layout style resolution via BoxStyleRef).
    pub fn pseudo_style(&self, id: NodeId, side: PseudoElement) -> Option<&ComputedStyle> {
        self.pseudo(id, side).map(|(s, _)| s)
    }

    /// True if any styled element declares a CSS transition (E17-M3). Gates the
    /// transition sampling pass so non-transition pages skip the second cascade.
    pub fn has_transitions(&self) -> bool {
        self.styles.values().any(|s| !s.transitions.is_empty())
    }
}

/// Build the styled tree at the default 800×600 viewport. Back-compat shim
/// (E13-M3): every existing caller stays unchanged; only `render_document`
/// threads the real viewport via `style_tree_vp`.
pub fn style_tree(doc: &Document, author_sheets: &[Stylesheet]) -> StyledTree {
    style_tree_vp(doc, author_sheets, Viewport::from_width(800.0))
}

/// Build the styled tree against a given [`Viewport`] (E13-M3). Walks the DOM
/// from the root, cascading each element against the UA sheet + the given author
/// stylesheets (their @media-active rules flattened in true source order),
/// applying inheritance. Infallible.
pub fn style_tree_vp(doc: &Document, author_sheets: &[Stylesheet], vp: Viewport) -> StyledTree {
    let ua = ua::ua_stylesheet();
    // Precedence-base order: UA first, then author sheets in given order. Each
    // sheet is pre-flattened to its viewport-active rules (top-level rules with
    // matching @media blocks interleaved at their source_index) ONCE here, so
    // every per-element cascade and the match cache see the same rule sequence.
    let mut active: Vec<(Origin, Vec<&Rule>)> = vec![(Origin::UserAgent, active_rules(&ua, vp))];
    for s in author_sheets {
        active.push((Origin::Author, active_rules(s, vp)));
    }

    // E11-M2: memoize per-element selector matches across the whole walk.
    let mut cache = CascadeCache::new(&active);

    let mut tree = StyledTree::default();
    let parent_initial = ComputedStyle::initial();
    let root_font_size = parent_initial.font_size;

    // E16-M1: live counter stack, threaded through the pre-order walk.
    let mut counters = counters::CounterState::default();

    // Pre-order DFS from the document root over element subtrees.
    let mut root_fs = root_font_size;
    for child in doc.children(doc.root()) {
        style_node(
            doc,
            child,
            &parent_initial,
            &active,
            vp,
            &mut root_fs,
            &mut tree,
            &mut cache,
            &mut counters,
        );
    }
    tree
}

/// Flatten one stylesheet to its viewport-active rules in TRUE source order: a
/// two-pointer merge of the top-level `rules` with the matching `media_blocks`
/// by `source_index`. A matching media block's rules are emitted just BEFORE the
/// top-level rule at its `source_index` (i.e. at the position where the @media
/// appeared in source). Non-matching blocks are excluded. With no media_blocks
/// this yields `sheet.rules.iter().collect()` in identical order (byte-identical
/// regression path).
fn active_rules(sheet: &Stylesheet, vp: Viewport) -> Vec<&Rule> {
    if sheet.media_blocks.is_empty() {
        return sheet.rules.iter().collect();
    }
    let mut out: Vec<&Rule> = Vec::new();
    let mut bi = 0; // next media block (blocks are in source order)
    for (idx, rule) in sheet.rules.iter().enumerate() {
        // Emit every media block opening at or before this rule's index.
        while bi < sheet.media_blocks.len() && sheet.media_blocks[bi].source_index <= idx {
            let mb = &sheet.media_blocks[bi];
            if media::media_matches(&mb.query, vp) {
                out.extend(mb.rules.iter());
            }
            bi += 1;
        }
        out.push(rule);
    }
    // Trailing media blocks (source_index == rules.len()).
    while bi < sheet.media_blocks.len() {
        let mb = &sheet.media_blocks[bi];
        if media::media_matches(&mb.query, vp) {
            out.extend(mb.rules.iter());
        }
        bi += 1;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn style_node(
    doc: &Document,
    node: NodeId,
    parent_style: &ComputedStyle,
    sheets: &[(Origin, Vec<&Rule>)],
    vp: Viewport,
    root_font_size: &mut f32,
    tree: &mut StyledTree,
    cache: &mut CascadeCache,
    counters: &mut counters::CounterState,
) {
    // Only element nodes are styled; descend through their children.
    if doc.tag_name(node).is_none() {
        return;
    }

    let mut style = parent_style.inherit_from();
    let ctx = EmContext {
        parent_font_size: parent_style.font_size,
        root_font_size: *root_font_size,
        viewport: vp,
    };
    cascade(doc, node, sheets, ctx, &mut style, cache);

    // The first styled element (the root element, e.g. <html>) defines `rem`.
    if doc.tag_name(node) == Some("html") {
        *root_font_size = style.font_size;
    }

    // E16-M1: apply this element's counter operations. `counter-reset` opens a
    // scope (pushed values popped after the subtree); `counter-increment`
    // accumulates and persists for later siblings.
    let pushed = counters.apply_reset(&style.counter_reset);
    counters.apply_increment(&style.counter_increment);

    // E7-M2: ::before / ::after generated-content pseudos. `counter()`/
    // `counters()` in their content read the now-updated counter state.
    for side in [PseudoElement::Before, PseudoElement::After] {
        if let Some(entry) = cascade_pseudo(doc, node, side, &style, sheets, ctx, &*counters) {
            match side {
                PseudoElement::Before => tree.before.insert(node, entry),
                PseudoElement::After => tree.after.insert(node, entry),
            };
        }
    }

    for child in doc.children(node) {
        style_node(
            doc,
            child,
            &style,
            sheets,
            vp,
            root_font_size,
            tree,
            cache,
            counters,
        );
    }

    // Close this element's reset scope (siblings keep accumulated increments).
    counters.undo_reset(pushed);

    tree.styles.insert(node, style);
}

// --- E17-M1: animation sampling pass ---

/// The animatable properties supported in E17-M1, in their canonical declaration
/// name. Used to scan a keyframe block for animatable declarations.
const ANIMATABLE: &[&str] = &[
    "opacity",
    "color",
    "background-color",
    "width",
    "height",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "top",
    "right",
    "bottom",
    "left",
    "transform",
    "border-color",
    "border-radius",
    "box-shadow",
];

/// Sample each animated element's [`Animation`] onto a static frame at
/// `at_seconds` (E17-M1). For every styled element with an `animation` whose
/// name matches a `@keyframes` rule, the supported animatable properties present
/// in the keyframes are interpolated at the eased progress and overwritten on the
/// element's [`ComputedStyle`]. Elements without an animation (or whose name
/// doesn't resolve) are left untouched, so a no-animation page is unchanged.
pub fn apply_animations(
    doc: &Document,
    author_sheets: &[Stylesheet],
    tree: &mut StyledTree,
    at_seconds: f32,
    vp: Viewport,
) {
    // name → keyframes rule (last definition wins).
    let mut kf_by_name: HashMap<&str, &KeyframesRule> = HashMap::new();
    for sheet in author_sheets {
        for kf in &sheet.keyframes {
            kf_by_name.insert(kf.name.as_str(), kf);
        }
    }
    if kf_by_name.is_empty() {
        return;
    }

    let root_font_size = tree
        .styles
        .iter()
        .find(|(id, _)| doc.tag_name(**id) == Some("html"))
        .map(|(_, s)| s.font_size)
        .unwrap_or_else(|| ComputedStyle::initial().font_size);

    let ids: Vec<NodeId> = tree.styles.keys().copied().collect();
    for id in ids {
        let anim = match tree.styles.get(&id).and_then(|s| s.animation.clone()) {
            Some(a) => a,
            None => continue,
        };
        let Some(kf) = kf_by_name.get(anim.name.as_str()) else {
            continue;
        };

        // Eased progress at this clock, honouring delay / iteration-count /
        // direction / fill-mode. `None` => no override applies (the cascaded
        // base value wins — fill-mode None outside the active span).
        let p = match resolve_progress(&anim, at_seconds) {
            Some(p) => p,
            None => continue,
        };

        // Sorted keyframe offsets for binary-ish pair search.
        let mut order: Vec<usize> = (0..kf.keyframes.len()).collect();
        order.sort_by(|&a, &b| {
            kf.keyframes[a]
                .offset
                .partial_cmp(&kf.keyframes[b].offset)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let ctx = EmContext {
            parent_font_size: tree.styles[&id].font_size,
            root_font_size,
            viewport: vp,
        };

        for prop in ANIMATABLE {
            // The keyframes (by sorted order) that declare this property.
            let frames: Vec<usize> = order
                .iter()
                .copied()
                .filter(|&i| kf.keyframes[i].declarations.iter().any(|d| d.name == *prop))
                .collect();
            if frames.is_empty() {
                continue;
            }

            // Surrounding pair by offset (clamp at the ends).
            let (lo_i, hi_i, local_t) = surrounding_pair(kf, &frames, p);
            let lo_decl = last_decl(&kf.keyframes[lo_i].declarations, prop);
            let hi_decl = last_decl(&kf.keyframes[hi_i].declarations, prop);
            let (Some(lo_decl), Some(hi_decl)) = (lo_decl, hi_decl) else {
                continue;
            };

            apply_interpolated(
                tree.styles.get_mut(&id).unwrap(),
                prop,
                lo_decl,
                hi_decl,
                local_t,
                ctx,
            );
        }
    }
}

/// Resolve the eased progress of `anim` at clock `at_seconds` (E17-M2), honouring
/// delay, iteration-count, direction and fill-mode. Returns `None` when no value
/// should be applied (fill-mode leaves the base value in place); `Some(p)` is the
/// eased progress fed to the keyframe interpolation.
///
/// Reduces EXACTLY to M1's `easing.eval((at/dur).clamp(0,1))` for the common
/// `name Ds easing` (delay 0, iter 1, Normal, fill None) over `at ∈ [0, dur]`,
/// with the boundary `at == dur` held at the last frame (progress 1.0).
fn resolve_progress(anim: &Animation, at_seconds: f32) -> Option<f32> {
    use AnimFillMode::*;
    let d = anim.duration_s;
    let delay = anim.delay_s;
    let n = anim.iteration_count;
    let easing = anim.timing;
    let dir = anim.direction;

    let t_active = at_seconds - delay;

    // Before the active span starts.
    if t_active < 0.0 {
        return match anim.fill_mode {
            Backwards | Both => Some(easing.eval(dir_progress(0, 0.0, dir))),
            None | Forwards => Option::None,
        };
    }

    let finite = n.is_finite();
    let end = n * d; // only meaningful when finite

    // After the active span ends (finite iteration count). The exact boundary
    // `t_active == end` is held (a single instant) regardless of fill-mode, to
    // preserve M1's "last frame at at==dur" behaviour.
    if finite && t_active >= end {
        let last_iter = (n.ceil() as u32).saturating_sub(1);
        let held = || Some(easing.eval(dir_progress(last_iter, 1.0, dir)));
        if t_active == end {
            return held();
        }
        return match anim.fill_mode {
            Forwards | Both => held(),
            None | Backwards => Option::None,
        };
    }

    // Within the active span (or infinite iteration count). A zero/negative
    // duration holds the last frame.
    if d <= 0.0 {
        let last_iter = if finite {
            (n.ceil() as u32).saturating_sub(1)
        } else {
            0
        };
        return Some(easing.eval(dir_progress(last_iter, 1.0, dir)));
    }

    let raw = t_active / d;
    let iter = raw.floor() as u32;
    let local = raw - iter as f32;
    Some(easing.eval(dir_progress(iter, local, dir)))
}

/// Map a per-iteration linear fraction `local` through the animation direction
/// for iteration index `iter` (E17-M2).
fn dir_progress(iter: u32, local: f32, dir: AnimDirection) -> f32 {
    use AnimDirection::*;
    let even = iter % 2 == 0;
    match dir {
        Normal => local,
        Reverse => 1.0 - local,
        Alternate => {
            if even {
                local
            } else {
                1.0 - local
            }
        }
        AlternateReverse => {
            if even {
                1.0 - local
            } else {
                local
            }
        }
    }
}

/// Find the keyframe pair (indices into `kf.keyframes`) surrounding progress `p`
/// among the given sorted `frames` (indices that declare the property), and the
/// local fraction within that span. Clamps to the first/last frame at the ends.
fn surrounding_pair(kf: &KeyframesRule, frames: &[usize], p: f32) -> (usize, usize, f32) {
    let off = |i: usize| kf.keyframes[i].offset;
    if p <= off(frames[0]) {
        return (frames[0], frames[0], 0.0);
    }
    let last = *frames.last().unwrap();
    if p >= off(last) {
        return (last, last, 0.0);
    }
    for w in frames.windows(2) {
        let (a, b) = (w[0], w[1]);
        if p >= off(a) && p <= off(b) {
            let span = off(b) - off(a);
            let t = if span <= 0.0 {
                0.0
            } else {
                (p - off(a)) / span
            };
            return (a, b, t);
        }
    }
    (last, last, 0.0)
}

/// The last declaration named `name` in a keyframe block (later wins).
fn last_decl<'a>(decls: &'a [Declaration], name: &str) -> Option<&'a Declaration> {
    decls.iter().rev().find(|d| d.name == name)
}

/// Parse each endpoint declaration via the cascade's `apply_declaration` (the
/// reuse trick: apply onto a scratch clone, read the resolved field), then lerp
/// and overwrite the field on `style`.
fn apply_interpolated(
    style: &mut ComputedStyle,
    prop: &str,
    lo: &Declaration,
    hi: &Declaration,
    t: f32,
    ctx: EmContext,
) {
    use interpolate::{lerp_f32, lerp_length, lerp_rgba};

    let custom = style.custom_props.clone();
    let mut scratch_lo = style.clone();
    properties::apply_declaration(&mut scratch_lo, lo, ctx, &custom);
    let mut scratch_hi = style.clone();
    properties::apply_declaration(&mut scratch_hi, hi, ctx, &custom);

    match prop {
        "opacity" => {
            style.opacity = lerp_f32(scratch_lo.opacity, scratch_hi.opacity, t).clamp(0.0, 1.0);
        }
        "color" => style.color = lerp_rgba(scratch_lo.color, scratch_hi.color, t),
        "background-color" => {
            style.background_color =
                lerp_rgba(scratch_lo.background_color, scratch_hi.background_color, t)
        }
        "width" => style.width = lerp_length(scratch_lo.width, scratch_hi.width, t),
        "height" => style.height = lerp_length(scratch_lo.height, scratch_hi.height, t),
        "margin-top" => {
            style.margin_top = lerp_length(scratch_lo.margin_top, scratch_hi.margin_top, t)
        }
        "margin-right" => {
            style.margin_right = lerp_length(scratch_lo.margin_right, scratch_hi.margin_right, t)
        }
        "margin-bottom" => {
            style.margin_bottom = lerp_length(scratch_lo.margin_bottom, scratch_hi.margin_bottom, t)
        }
        "margin-left" => {
            style.margin_left = lerp_length(scratch_lo.margin_left, scratch_hi.margin_left, t)
        }
        "padding-top" => {
            style.padding_top = lerp_length(scratch_lo.padding_top, scratch_hi.padding_top, t)
        }
        "padding-right" => {
            style.padding_right = lerp_length(scratch_lo.padding_right, scratch_hi.padding_right, t)
        }
        "padding-bottom" => {
            style.padding_bottom =
                lerp_length(scratch_lo.padding_bottom, scratch_hi.padding_bottom, t)
        }
        "padding-left" => {
            style.padding_left = lerp_length(scratch_lo.padding_left, scratch_hi.padding_left, t)
        }
        "top" => style.top = lerp_length(scratch_lo.top, scratch_hi.top, t),
        "right" => style.right = lerp_length(scratch_lo.right, scratch_hi.right, t),
        "bottom" => style.bottom = lerp_length(scratch_lo.bottom, scratch_hi.bottom, t),
        "left" => style.left = lerp_length(scratch_lo.left, scratch_hi.left, t),
        "transform" => {
            style.transform =
                interpolate::lerp_transform(&scratch_lo.transform, &scratch_hi.transform, t);
        }
        "border-color" => {
            style.border_color = lerp_rgba(scratch_lo.border_color, scratch_hi.border_color, t)
        }
        "border-radius" => {
            style.border_radius =
                interpolate::lerp_radius(scratch_lo.border_radius, scratch_hi.border_radius, t)
        }
        "box-shadow" => {
            style.box_shadow =
                interpolate::lerp_box_shadow(scratch_lo.box_shadow, scratch_hi.box_shadow, t)
        }
        _ => {}
    }
}

// --- E17-M3: one-shot transition sampling ---

/// The transitionable properties supported in E17-M3 (canonical names). Each
/// element's `transitions` list watches a subset of these via `TransitionProp`.
const TRANSITIONABLE: &[&str] = &[
    "opacity",
    "color",
    "background-color",
    "border-color",
    "width",
    "height",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "top",
    "right",
    "bottom",
    "left",
    "border-radius",
    "box-shadow",
    "transform",
];

/// Sample one-shot CSS transitions onto the current (`to`) styled tree (E17-M3).
/// For every element with a non-empty `transitions` list whose pre-script (`from`)
/// style is known, each watched property whose `from` and `to` values differ is
/// overwritten with the eased interpolation at `at_seconds`. At progress 0 the
/// value equals `from`; at progress 1 it equals `to` (a no-op).
///
/// Applied AFTER `apply_animations`, so an animation on a shared property wins.
pub fn apply_transitions(
    doc: &Document,
    from_tree: &StyledTree,
    tree: &mut StyledTree,
    at_seconds: f32,
    _vp: Viewport,
) {
    let _ = doc;
    let ids: Vec<NodeId> = tree.styles.keys().copied().collect();
    for id in ids {
        let transitions = match tree.styles.get(&id) {
            Some(s) if !s.transitions.is_empty() => s.transitions.clone(),
            _ => continue,
        };
        // The element must have existed pre-script for a from→to to be defined.
        let Some(from) = from_tree.styles.get(&id) else {
            continue;
        };
        let from = from.clone();

        for prop in TRANSITIONABLE {
            // Last transition entry that watches this property wins.
            let entry = transitions.iter().rev().find(|tr| match &tr.property {
                TransitionProp::All => true,
                TransitionProp::Name(n) => n == prop,
            });
            let Some(entry) = entry else { continue };
            let p = resolve_transition_progress(entry, at_seconds);
            let to = tree.styles.get_mut(&id).unwrap();
            lerp_field(to, prop, &from, p);
        }
    }
}

/// Eased progress of one transition `entry` at clock `at_seconds` (E17-M3):
/// before the delay → 0; at/after `delay + duration` (or zero duration) → 1;
/// otherwise the eased local fraction.
fn resolve_transition_progress(entry: &Transition, at_seconds: f32) -> f32 {
    let te = at_seconds - entry.delay_s;
    if te <= 0.0 {
        0.0
    } else if entry.duration_s <= 0.0 || te >= entry.duration_s {
        1.0
    } else {
        entry.timing.eval(te / entry.duration_s)
    }
}

/// Interpolate one property from `from` toward `to`'s current value at fraction
/// `t`, writing the result back onto `to`. Only overwrites when the endpoints
/// differ (so an unchanged property stays byte-identical). Shares the
/// `interpolate` helpers with `apply_interpolated`.
fn lerp_field(to: &mut ComputedStyle, prop: &str, from: &ComputedStyle, t: f32) {
    use interpolate::{lerp_f32, lerp_length, lerp_rgba};

    // Each arm binds `a` (from) / `b` (to) and overwrites the field only when the
    // endpoints differ; `$new` computes the interpolated value from `a`/`b`/`t`.
    macro_rules! lerp_f {
        ($field:ident, |$a:ident, $b:ident| $new:expr) => {{
            let $a = &from.$field;
            let $b = &to.$field;
            if $a != $b {
                to.$field = $new;
            }
        }};
    }

    match prop {
        "opacity" => lerp_f!(opacity, |a, b| lerp_f32(*a, *b, t).clamp(0.0, 1.0)),
        "color" => lerp_f!(color, |a, b| lerp_rgba(*a, *b, t)),
        "background-color" => lerp_f!(background_color, |a, b| lerp_rgba(*a, *b, t)),
        "border-color" => lerp_f!(border_color, |a, b| lerp_rgba(*a, *b, t)),
        "width" => lerp_f!(width, |a, b| lerp_length(*a, *b, t)),
        "height" => lerp_f!(height, |a, b| lerp_length(*a, *b, t)),
        "margin-top" => lerp_f!(margin_top, |a, b| lerp_length(*a, *b, t)),
        "margin-right" => lerp_f!(margin_right, |a, b| lerp_length(*a, *b, t)),
        "margin-bottom" => lerp_f!(margin_bottom, |a, b| lerp_length(*a, *b, t)),
        "margin-left" => lerp_f!(margin_left, |a, b| lerp_length(*a, *b, t)),
        "padding-top" => lerp_f!(padding_top, |a, b| lerp_length(*a, *b, t)),
        "padding-right" => lerp_f!(padding_right, |a, b| lerp_length(*a, *b, t)),
        "padding-bottom" => lerp_f!(padding_bottom, |a, b| lerp_length(*a, *b, t)),
        "padding-left" => lerp_f!(padding_left, |a, b| lerp_length(*a, *b, t)),
        "top" => lerp_f!(top, |a, b| lerp_length(*a, *b, t)),
        "right" => lerp_f!(right, |a, b| lerp_length(*a, *b, t)),
        "bottom" => lerp_f!(bottom, |a, b| lerp_length(*a, *b, t)),
        "left" => lerp_f!(left, |a, b| lerp_length(*a, *b, t)),
        "border-radius" => lerp_f!(border_radius, |a, b| interpolate::lerp_radius(*a, *b, t)),
        "box-shadow" => lerp_f!(box_shadow, |a, b| interpolate::lerp_box_shadow(*a, *b, t)),
        "transform" => lerp_f!(transform, |a, b| interpolate::lerp_transform(a, b, t)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_css::parse_stylesheet;
    use starfish_dom::NodeKind;
    use starfish_html::parse;

    /// Find the first element with the given tag in document order.
    fn find(doc: &Document, tag: &str) -> NodeId {
        find_opt(doc, tag).unwrap_or_else(|| panic!("no <{tag}> found"))
    }

    fn find_opt(doc: &Document, tag: &str) -> Option<NodeId> {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.tag_name(n) == Some(tag) {
                return Some(n);
            }
            // push children (reverse for document order isn't required here)
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        None
    }

    /// Find the first element with a given id.
    fn find_id(doc: &Document, id: &str) -> NodeId {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.get_attribute(n, "id") == Some(id) {
                return n;
            }
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        panic!("no element with id={id}")
    }

    fn red() -> Rgba {
        Rgba {
            r: 255,
            g: 0,
            b: 0,
            a: 255,
        }
    }
    fn blue() -> Rgba {
        Rgba {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        }
    }
    fn green() -> Rgba {
        Rgba {
            r: 0,
            g: 128,
            b: 0,
            a: 255,
        }
    }
    fn black() -> Rgba {
        Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        }
    }

    fn style(html: &str, css: &str) -> (Document, StyledTree) {
        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let tree = style_tree(&doc, &[sheet]);
        (doc, tree)
    }

    // --- §9.1 selector matching (exercised end-to-end via the cascade) ---

    #[test]
    fn match_tag() {
        let (doc, t) = style("<p>hi</p>", "p { color: red }");
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    #[test]
    fn match_class() {
        let (doc, t) = style("<p class='x'>hi</p>", ".x { color: red }");
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    #[test]
    fn match_id() {
        let (doc, t) = style("<p id='m'>hi</p>", "#m { color: red }");
        assert_eq!(t.computed(find_id(&doc, "m")).color, red());
    }

    #[test]
    fn match_compound() {
        let (doc, t) = style(
            "<p class='a b' id='m'>hi</p>",
            "p.a.b#m { color: red } p.a.d { color: blue }",
        );
        // p.a.b#m matches; p.a.d does not.
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    #[test]
    fn match_multiclass_subset() {
        let (doc, t) = style("<p class='a b c'>hi</p>", ".a.c { color: red }");
        assert_eq!(t.computed(find(&doc, "p")).color, red());
        let (doc2, t2) = style("<p class='a b c'>hi</p>", ".a.d { color: red }");
        // .a.d does not match → stays initial black.
        assert_eq!(t2.computed(find(&doc2, "p")).color, black());
    }

    #[test]
    fn match_descendant() {
        let (doc, t) = style(
            "<div><section><p>hi</p></section></div>",
            "div p { color: red }",
        );
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    #[test]
    fn no_match_descendant_without_ancestor() {
        let (doc, t) = style("<section><p>hi</p></section>", "div p { color: red }");
        assert_eq!(t.computed(find(&doc, "p")).color, black());
    }

    #[test]
    fn descendant_backtracking() {
        // Selector `x > .b .c`. DOM has two `.b` ancestors of `.c`: the nearest
        // (inner) `.b` is NOT a direct child of `x`, so a greedy matcher that
        // commits to it for the `x > .b` segment would fail. A correct matcher
        // must backtrack to the outer `.b`, which IS `x`'s child → matches.
        let html = "<x><div class='b'><div class='wrap'>\
            <div class='b'><div class='c'>z</div></div>\
            </div></div></x>";
        let (doc, t) = style(html, "x > .b .c { color: red }");
        assert_eq!(t.computed(find_class(&doc, "c")).color, red());
    }

    #[test]
    fn descendant_backtracking_negative() {
        // Same shape as `descendant_backtracking` but the outer `.b` no longer
        // has an `x` parent, so no `.b` ancestor of `.c` is a child of `x`.
        // Even with backtracking, the selector must NOT match.
        let html = "<div class='b'><div class='wrap'>\
            <div class='b'><div class='c'>z</div></div>\
            </div></div>";
        let (doc, t) = style(html, "x > .b .c { color: red }");
        assert_eq!(t.computed(find_class(&doc, "c")).color, black());
    }

    fn find_class(doc: &Document, cls: &str) -> NodeId {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if let Some(c) = doc.get_attribute(n, "class") {
                if c.split_ascii_whitespace().any(|x| x == cls) {
                    return n;
                }
            }
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        panic!("no .{cls}")
    }

    #[test]
    fn child_combinator() {
        // div > p matches direct child only.
        let (doc, t) = style(
            "<div><p id='direct'>a</p><section><p id='grand'>b</p></section></div>",
            "div > p { color: red }",
        );
        assert_eq!(t.computed(find_id(&doc, "direct")).color, red());
        assert_eq!(t.computed(find_id(&doc, "grand")).color, black());
    }

    #[test]
    fn rule_max_specificity_over_selectors() {
        // A rule with selectors `p` and `p.win` — its specificity for a matching
        // .win element is the max, so it beats a competing lower-spec rule that
        // comes later in source order.
        let (doc, t) = style(
            "<p class='win'>x</p>",
            "p, p.win { color: red } p { color: blue }",
        );
        // first rule (max spec 0,1,1) beats later plain `p` (0,0,1).
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    // --- §9.2 cascade ---

    #[test]
    fn source_order_tiebreak() {
        let (doc, t) = style("<p>x</p>", "p { color: red } p { color: blue }");
        assert_eq!(t.computed(find(&doc, "p")).color, blue());
    }

    #[test]
    fn specificity_beats_order() {
        let (doc, t) = style(
            "<p id='m' class='c'>x</p>",
            "#m { color: red } .c { color: blue } p { color: green }",
        );
        // #id (1,0,0) wins regardless of order.
        assert_eq!(t.computed(find_id(&doc, "m")).color, red());
    }

    #[test]
    fn important_beats_higher_specificity() {
        let (doc, t) = style(
            "<p id='m' class='c'>x</p>",
            "#m { color: red } .c { color: blue !important }",
        );
        assert_eq!(t.computed(find_id(&doc, "m")).color, blue());
    }

    #[test]
    fn explicit_border_color_not_clobbered_by_current_color() {
        // color and border-color differ; the currentColor fallback must NOT
        // overwrite the explicitly-set border-color.
        let (doc, t) = style("<p>x</p>", "p { color: red; border-color: blue }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.color, red());
        assert_eq!(p.border_color, blue());
    }

    #[test]
    fn author_overrides_ua_display() {
        let (doc, t) = style("<div>x</div>", "div { display: inline }");
        assert_eq!(t.computed(find(&doc, "div")).display, Display::Inline);
    }

    // --- §9.3 inheritance ---

    #[test]
    fn color_inherits() {
        let (doc, t) = style(
            "<body><div><span>hi</span></div></body>",
            "body { color: red }",
        );
        assert_eq!(t.computed(find(&doc, "span")).color, red());
    }

    #[test]
    fn background_not_inherited() {
        let (doc, t) = style(
            "<body><span>hi</span></body>",
            "body { background-color: red }",
        );
        let transparent = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
        let s = t.computed(find(&doc, "span"));
        assert_eq!(s.background_color, transparent);
        assert!(s.background_layers.is_empty());
    }

    #[test]
    fn font_size_inherits_and_em_resolves() {
        let (doc, t) = style(
            "<div><p>hi</p></div>",
            "div { font-size: 20px } p { margin: 2em }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.font_size, 20.0);
        // 2em against p's own font-size (20) = 40.
        assert_eq!(p.margin_top, Length::Px(40.0));
    }

    #[test]
    fn font_size_percent_resolves_against_parent() {
        let (doc, t) = style(
            "<div><p>hi</p></div>",
            "div { font-size: 20px } p { font-size: 150% }",
        );
        assert_eq!(t.computed(find(&doc, "p")).font_size, 30.0);
    }

    #[test]
    fn rem_resolves_against_root() {
        let (doc, t) = style(
            "<html><body><p>hi</p></body></html>",
            "html { font-size: 10px } p { width: 3rem }",
        );
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Px(30.0));
    }

    // --- §9.4 property parsing ---

    #[test]
    fn margin_shorthand_forms() {
        let (doc, t) = style("<p>x</p>", "p { margin: 10px }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.margin_top, Length::Px(10.0));
        assert_eq!(p.margin_left, Length::Px(10.0));

        let (doc2, t2) = style("<p>x</p>", "p { margin: 1px 2px }");
        let p2 = t2.computed(find(&doc2, "p"));
        assert_eq!(p2.margin_top, Length::Px(1.0));
        assert_eq!(p2.margin_bottom, Length::Px(1.0));
        assert_eq!(p2.margin_right, Length::Px(2.0));
        assert_eq!(p2.margin_left, Length::Px(2.0));

        let (doc3, t3) = style("<p>x</p>", "p { margin: 1px 2px 3px 4px }");
        let p3 = t3.computed(find(&doc3, "p"));
        assert_eq!(p3.margin_top, Length::Px(1.0));
        assert_eq!(p3.margin_right, Length::Px(2.0));
        assert_eq!(p3.margin_bottom, Length::Px(3.0));
        assert_eq!(p3.margin_left, Length::Px(4.0));
    }

    #[test]
    fn padding_auto_becomes_zero() {
        let (doc, t) = style("<p>x</p>", "p { padding: auto }");
        assert_eq!(t.computed(find(&doc, "p")).padding_top, Length::Px(0.0));
    }

    #[test]
    fn width_percent_auto_zero() {
        let (doc, t) = style("<p>x</p>", "p { width: 50% }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Percent(50.0));
        let (doc2, t2) = style("<p>x</p>", "p { width: auto }");
        assert_eq!(t2.computed(find(&doc2, "p")).width, Length::Auto);
        let (doc3, t3) = style("<p>x</p>", "p { height: 0 }");
        assert_eq!(t3.computed(find(&doc3, "p")).height, Length::Px(0.0));
    }

    // --- E13-M1: box-sizing + min/max parsing ---

    #[test]
    fn box_sizing_parses() {
        use crate::computed::BoxSizing;
        let (doc, t) = style("<p>x</p>", "p { box-sizing: border-box }");
        assert_eq!(t.computed(find(&doc, "p")).box_sizing, BoxSizing::BorderBox);
        let (doc2, t2) = style("<p>x</p>", "p { box-sizing: content-box }");
        assert_eq!(
            t2.computed(find(&doc2, "p")).box_sizing,
            BoxSizing::ContentBox
        );
        // initial / unknown keyword → ContentBox unchanged.
        let (doc3, t3) = style("<p>x</p>", "p { box-sizing: bogus }");
        assert_eq!(
            t3.computed(find(&doc3, "p")).box_sizing,
            BoxSizing::ContentBox
        );
    }

    #[test]
    fn min_max_size_parses() {
        let (doc, t) = style(
            "<p>x</p>",
            "p { min-width: 50px; max-width: 200px; min-height: 10px; max-height: 80% }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.min_width, Length::Px(50.0));
        assert_eq!(p.max_width, Length::Px(200.0));
        assert_eq!(p.min_height, Length::Px(10.0));
        assert_eq!(p.max_height, Length::Percent(80.0));
    }

    #[test]
    fn max_size_none_and_auto_sentinels() {
        // `none` is the "no maximum" sentinel = Auto; explicit `auto` on max-* is
        // invalid and leaves the initial Auto.
        let (doc, t) = style("<p>x</p>", "p { max-width: none; max-height: auto }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.max_width, Length::Auto);
        assert_eq!(p.max_height, Length::Auto);
        // min-* default is Auto (means 0).
        assert_eq!(p.min_width, Length::Auto);
    }

    #[test]
    fn border_shorthand() {
        let (doc, t) = style("<p>x</p>", "p { border: 2px solid red }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.border_top_width, 2.0);
        assert_eq!(p.border_left_width, 2.0);
        assert_eq!(p.border_style, BorderStyle::Solid);
        assert_eq!(p.border_color, red());
    }

    #[test]
    fn font_weight_values() {
        let (doc, t) = style("<p>x</p>", "p { font-weight: bold }");
        assert_eq!(t.computed(find(&doc, "p")).font_weight, FontWeight(700));
        let (doc2, t2) = style("<p>x</p>", "p { font-weight: 300 }");
        assert_eq!(t2.computed(find(&doc2, "p")).font_weight, FontWeight(300));
    }

    #[test]
    fn line_height_forms() {
        let (doc, t) = style("<p>x</p>", "p { line-height: 1.5 }");
        assert_eq!(
            t.computed(find(&doc, "p")).line_height,
            LineHeight::Number(1.5)
        );
        let (doc2, t2) = style("<p>x</p>", "p { line-height: 20px }");
        assert_eq!(
            t2.computed(find(&doc2, "p")).line_height,
            LineHeight::Px(20.0)
        );
        let (doc3, t3) = style("<p>x</p>", "p { line-height: normal }");
        assert_eq!(
            t3.computed(find(&doc3, "p")).line_height,
            LineHeight::Normal
        );
    }

    #[test]
    fn font_family_list() {
        let (doc, t) = style(
            "<p>x</p>",
            "p { font-family: \"Helvetica Neue\", Arial, sans-serif }",
        );
        assert_eq!(
            t.computed(find(&doc, "p")).font_family,
            vec!["Helvetica Neue", "Arial", "sans-serif"]
        );
    }

    #[test]
    fn font_style_values() {
        let (doc, t) = style("<p>x</p>", "p { font-style: italic }");
        assert_eq!(t.computed(find(&doc, "p")).font_style, FontStyle::Italic);
        let (doc2, t2) = style("<p>x</p>", "p { font-style: oblique }");
        assert_eq!(t2.computed(find(&doc2, "p")).font_style, FontStyle::Oblique);
        let (doc3, t3) = style("<p>x</p>", "p { font-style: normal }");
        assert_eq!(t3.computed(find(&doc3, "p")).font_style, FontStyle::Normal);
        // `oblique 14deg` → Oblique (angle ignored).
        let (doc4, t4) = style("<p>x</p>", "p { font-style: oblique 14deg }");
        assert_eq!(t4.computed(find(&doc4, "p")).font_style, FontStyle::Oblique);
    }

    #[test]
    fn font_style_inherited() {
        // a child <span> with no font-style under an italic <p> is Italic.
        let (doc, t) = style("<p>a<span>b</span></p>", "p { font-style: italic }");
        assert_eq!(t.computed(find(&doc, "span")).font_style, FontStyle::Italic);
    }

    #[test]
    fn color_hex_and_transparent_bg() {
        let (doc, t) = style("<p>x</p>", "p { color: #00ff00 }");
        assert_eq!(
            t.computed(find(&doc, "p")).color,
            Rgba {
                r: 0,
                g: 255,
                b: 0,
                a: 255
            }
        );
        let (doc2, t2) = style("<p>x</p>", "p { background-color: transparent }");
        assert_eq!(
            t2.computed(find(&doc2, "p")).background_color,
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0
            }
        );
    }

    #[test]
    fn unknown_and_bogus_ignored() {
        let (doc, t) = style("<p>x</p>", "p { zoom: 2; width: bogus }");
        // width unchanged from initial Auto; no panic.
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Auto);
    }

    // --- §9.5 UA sheet + smoke ---

    #[test]
    fn ua_display_defaults() {
        let (doc, t) = style("<div><span>hi</span></div>", "");
        assert_eq!(t.computed(find(&doc, "div")).display, Display::Block);
        assert_eq!(t.computed(find(&doc, "span")).display, Display::Inline);
        // both inherit black + 16px.
        assert_eq!(t.computed(find(&doc, "span")).color, black());
        assert_eq!(t.computed(find(&doc, "span")).font_size, 16.0);
    }

    #[test]
    fn ua_head_none() {
        let doc = parse("<html><head><title>t</title></head><body>x</body></html>");
        let t = style_tree(&doc, &[]);
        assert_eq!(t.computed(find(&doc, "head")).display, Display::None);
        if let Some(title) = find_opt(&doc, "title") {
            assert_eq!(t.computed(title).display, Display::None);
        }
    }

    #[test]
    fn ua_body_margin() {
        let doc = parse("<body>x</body>");
        let t = style_tree(&doc, &[]);
        assert_eq!(t.computed(find(&doc, "body")).margin_top, Length::Px(8.0));
    }

    #[test]
    fn ua_p_margin_with_author_color() {
        let (doc, t) = style("<p>hi</p>", "p { color: blue }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.display, Display::Block);
        // UA margin 16px 0 retained.
        assert_eq!(p.margin_top, Length::Px(16.0));
        assert_eq!(p.margin_left, Length::Px(0.0));
        assert_eq!(p.color, blue());
    }

    #[test]
    fn ua_bold_for_strong() {
        let (doc, t) = style("<strong>x</strong>", "");
        assert_eq!(
            t.computed(find(&doc, "strong")).font_weight,
            FontWeight(700)
        );
    }

    #[test]
    fn text_node_has_no_entry() {
        let (doc, t) = style("<p>hi</p>", "");
        let p = find(&doc, "p");
        let text = doc
            .children(p)
            .into_iter()
            .find(|c| matches!(doc.kind(*c), NodeKind::Text(_)))
            .expect("text node");
        assert!(t.get(text).is_none());
    }

    #[test]
    fn smoke_small_page() {
        let html = "<html><body>\
            <h1 id='title'>Hello</h1>\
            <div class='box'><p>Para</p><ul><li><a href='#'>link</a></li></ul></div>\
            </body></html>";
        let css = "body { font-family: Arial } \
                   .box { background-color: #ff0000; border: 1px solid #0000ff } \
                   p { color: green } a { color: blue }";
        let (doc, t) = style(html, css);

        let h1 = t.computed(find_id(&doc, "title"));
        assert_eq!(h1.display, Display::Block);
        assert_eq!(h1.font_size, 32.0); // UA h1
        assert_eq!(h1.font_weight, FontWeight(700));

        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.color, green());
        assert_eq!(p.display, Display::Block);
        assert_eq!(p.font_family, vec!["Arial"]); // inherited from body

        let box_el = find_class(&doc, "box");
        let b = t.computed(box_el);
        assert_eq!(b.background_color, red());
        assert_eq!(b.border_top_width, 1.0);
        assert_eq!(b.border_style, BorderStyle::Solid);
        assert_eq!(b.border_color, blue());

        let a = t.computed(find(&doc, "a"));
        assert_eq!(a.display, Display::Inline);
        assert_eq!(a.color, blue());

        let ul = t.computed(find(&doc, "ul"));
        assert_eq!(ul.padding_left, Length::Px(40.0));
    }

    // --- E2-M1: text-decoration / list-style ---

    #[test]
    fn text_decoration_underline() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration: underline }");
        let d = t.computed(find(&doc, "p")).text_decoration_line;
        assert!(d.contains(TextDecorationLine::UNDERLINE));
        assert!(!d.contains(TextDecorationLine::OVERLINE));
        assert!(!d.contains(TextDecorationLine::LINE_THROUGH));
    }

    #[test]
    fn text_decoration_line_combines() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration-line: underline overline }");
        let d = t.computed(find(&doc, "p")).text_decoration_line;
        assert!(d.contains(TextDecorationLine::UNDERLINE));
        assert!(d.contains(TextDecorationLine::OVERLINE));
        assert!(!d.contains(TextDecorationLine::LINE_THROUGH));
    }

    #[test]
    fn text_decoration_none() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration: none }");
        assert!(t.computed(find(&doc, "p")).text_decoration_line.is_none());
    }

    #[test]
    fn text_decoration_shorthand_ignores_color_style() {
        // `underline` line keyword honored; `solid`/color ignored (M1).
        let (doc, t) = style("<p>x</p>", "p { text-decoration: underline solid red }");
        let d = t.computed(find(&doc, "p")).text_decoration_line;
        assert!(d.contains(TextDecorationLine::UNDERLINE));
    }

    #[test]
    fn list_style_type_values() {
        let (doc, t) = style("<ul><li>a</li></ul>", "li { list-style-type: square }");
        assert_eq!(
            t.computed(find(&doc, "li")).list_style_type,
            ListStyleType::Square
        );
        let (doc2, t2) = style("<ul><li>a</li></ul>", "li { list-style-type: none }");
        assert_eq!(
            t2.computed(find(&doc2, "li")).list_style_type,
            ListStyleType::None
        );
    }

    #[test]
    fn list_style_shorthand_type() {
        let (doc, t) = style("<ul><li>a</li></ul>", "ul { list-style: circle }");
        assert_eq!(
            t.computed(find(&doc, "ul")).list_style_type,
            ListStyleType::Circle
        );
    }

    #[test]
    fn ua_list_style_defaults() {
        let (doc, t) = style("<ul><li>a</li></ul><ol><li>b</li></ol>", "");
        assert_eq!(
            t.computed(find(&doc, "ul")).list_style_type,
            ListStyleType::Disc
        );
        assert_eq!(
            t.computed(find(&doc, "ol")).list_style_type,
            ListStyleType::Decimal
        );
    }

    #[test]
    fn list_style_inherits_to_li() {
        let (doc, t) = style("<ul><li>a</li></ul>", "ul { list-style-type: square }");
        // <li> has no own list-style-type → inherits the <ul>'s computed Square.
        assert_eq!(
            t.computed(find(&doc, "li")).list_style_type,
            ListStyleType::Square
        );
    }

    #[test]
    fn text_decoration_not_inherited() {
        let (doc, t) = style("<p>a<span>b</span></p>", "p { text-decoration: underline }");
        // span does not inherit the parent's text-decoration-line.
        assert!(t
            .computed(find(&doc, "span"))
            .text_decoration_line
            .is_none());
    }

    // --- E2-M2: position / float / clear / offsets ---

    #[test]
    fn position_values() {
        use Position::*;
        for (kw, want) in [
            ("static", Static),
            ("relative", Relative),
            ("absolute", Absolute),
            ("fixed", Fixed),
            ("sticky", Sticky),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ position: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).position, want);
        }
    }

    #[test]
    fn position_bogus_stays_static() {
        let (doc, t) = style("<p>x</p>", "p { position: sticky-ish }");
        assert_eq!(t.computed(find(&doc, "p")).position, Position::Static);
    }

    #[test]
    fn float_values() {
        let (doc, t) = style("<p>x</p>", "p { float: left }");
        assert_eq!(t.computed(find(&doc, "p")).float, Float::Left);
        let (doc2, t2) = style("<p>x</p>", "p { float: right }");
        assert_eq!(t2.computed(find(&doc2, "p")).float, Float::Right);
        let (doc3, t3) = style("<p>x</p>", "p { float: none }");
        assert_eq!(t3.computed(find(&doc3, "p")).float, Float::None);
    }

    #[test]
    fn clear_both() {
        let (doc, t) = style("<p>x</p>", "p { clear: both }");
        assert_eq!(t.computed(find(&doc, "p")).clear, Clear::Both);
    }

    #[test]
    fn offset_lengths_incl_negative_and_percent() {
        let (doc, t) = style(
            "<p>x</p>",
            "p { top: 10px; left: -5px; right: 50%; bottom: auto }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.top, Length::Px(10.0));
        assert_eq!(p.left, Length::Px(-5.0));
        assert_eq!(p.right, Length::Percent(50.0));
        assert_eq!(p.bottom, Length::Auto);
    }

    #[test]
    fn positioning_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { position: relative; float: left; clear: both; top: 5px }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.position, Position::Static);
        assert_eq!(p.float, Float::None);
        assert_eq!(p.clear, Clear::None);
        assert_eq!(p.top, Length::Auto);
    }

    // --- E16-M4: text-overflow ---

    #[test]
    fn text_overflow_values() {
        let (doc, t) = style("<p>x</p>", "p { text-overflow: ellipsis }");
        assert_eq!(
            t.computed(find(&doc, "p")).text_overflow,
            TextOverflow::Ellipsis
        );
        let (doc2, t2) = style("<p>x</p>", "p { text-overflow: clip }");
        assert_eq!(
            t2.computed(find(&doc2, "p")).text_overflow,
            TextOverflow::Clip
        );
    }

    #[test]
    fn text_overflow_not_inherited() {
        let (doc, t) = style("<p>a<span>b</span></p>", "p { text-overflow: ellipsis }");
        // The child does not inherit text-overflow → stays at the initial Clip.
        assert_eq!(
            t.computed(find(&doc, "span")).text_overflow,
            TextOverflow::Clip
        );
    }

    #[test]
    fn text_overflow_initial_is_clip() {
        let (doc, t) = style("<p>x</p>", "");
        assert_eq!(
            t.computed(find(&doc, "p")).text_overflow,
            TextOverflow::Clip
        );
    }

    // --- E2-M3: flex ---

    #[test]
    fn display_flex_and_inline_flex() {
        let (doc, t) = style("<div>x</div>", "div { display: flex }");
        assert_eq!(t.computed(find(&doc, "div")).display, Display::Flex);
        let (doc2, t2) = style("<div>x</div>", "div { display: inline-flex }");
        assert_eq!(t2.computed(find(&doc2, "div")).display, Display::InlineFlex);
    }

    #[test]
    fn flex_direction_and_bogus_default() {
        let (doc, t) = style("<div>x</div>", "div { flex-direction: column-reverse }");
        assert_eq!(
            t.computed(find(&doc, "div")).flex_direction,
            FlexDirection::ColumnReverse
        );
        // bogus → keeps initial Row.
        let (doc2, t2) = style("<div>x</div>", "div { flex-direction: sideways }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).flex_direction,
            FlexDirection::Row
        );
    }

    #[test]
    fn flex_wrap_wrap_and_wrap_reverse_deferred() {
        let (doc, t) = style("<div>x</div>", "div { flex-wrap: wrap }");
        assert_eq!(t.computed(find(&doc, "div")).flex_wrap, FlexWrap::Wrap);
        // wrap-reverse deferred → ignored, stays Nowrap.
        let (doc2, t2) = style("<div>x</div>", "div { flex-wrap: wrap-reverse }");
        assert_eq!(t2.computed(find(&doc2, "div")).flex_wrap, FlexWrap::Nowrap);
    }

    #[test]
    fn justify_content_values() {
        let (doc, t) = style("<div>x</div>", "div { justify-content: space-between }");
        assert_eq!(
            t.computed(find(&doc, "div")).justify_content,
            JustifyContent::SpaceBetween
        );
        let (doc2, t2) = style("<div>x</div>", "div { justify-content: space-evenly }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).justify_content,
            JustifyContent::SpaceEvenly
        );
    }

    #[test]
    fn align_items_and_self() {
        let (doc, t) = style("<div>x</div>", "div { align-items: center }");
        assert_eq!(
            t.computed(find(&doc, "div")).align_items,
            AlignItems::Center
        );
        let (doc2, t2) = style("<div>x</div>", "div { align-self: flex-end }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).align_self,
            AlignSelf::FlexEnd
        );
        // default align-self is Auto.
        let (doc3, t3) = style("<div>x</div>", "div { color: red }");
        assert_eq!(t3.computed(find(&doc3, "div")).align_self, AlignSelf::Auto);
    }

    #[test]
    fn flex_longhands() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { flex-grow: 2; flex-shrink: 0; flex-basis: 100px }",
        );
        let d = t.computed(find(&doc, "div"));
        assert_eq!(d.flex_grow, 2.0);
        assert_eq!(d.flex_shrink, 0.0);
        assert_eq!(d.flex_basis, Length::Px(100.0));
        // defaults.
        let (doc2, t2) = style("<div>x</div>", "div { color: red }");
        let d2 = t2.computed(find(&doc2, "div"));
        assert_eq!(d2.flex_grow, 0.0);
        assert_eq!(d2.flex_shrink, 1.0);
        assert_eq!(d2.flex_basis, Length::Auto);
    }

    #[test]
    fn flex_shorthand_forms() {
        let (doc, t) = style("<div>x</div>", "div { flex: none }");
        let d = t.computed(find(&doc, "div"));
        assert_eq!(
            (d.flex_grow, d.flex_shrink, d.flex_basis),
            (0.0, 0.0, Length::Auto)
        );

        let (doc2, t2) = style("<div>x</div>", "div { flex: auto }");
        let d2 = t2.computed(find(&doc2, "div"));
        assert_eq!(
            (d2.flex_grow, d2.flex_shrink, d2.flex_basis),
            (1.0, 1.0, Length::Auto)
        );

        // single number = grow; omitted basis defaults to 0.
        let (doc3, t3) = style("<div>x</div>", "div { flex: 1 }");
        let d3 = t3.computed(find(&doc3, "div"));
        assert_eq!(
            (d3.flex_grow, d3.flex_shrink, d3.flex_basis),
            (1.0, 1.0, Length::Px(0.0))
        );

        let (doc4, t4) = style("<div>x</div>", "div { flex: 2 3 40px }");
        let d4 = t4.computed(find(&doc4, "div"));
        assert_eq!(
            (d4.flex_grow, d4.flex_shrink, d4.flex_basis),
            (2.0, 3.0, Length::Px(40.0))
        );
    }

    #[test]
    fn gap_shorthand_and_longhands() {
        let (doc, t) = style("<div>x</div>", "div { gap: 10px }");
        let d = t.computed(find(&doc, "div"));
        assert_eq!(d.row_gap, Length::Px(10.0));
        assert_eq!(d.column_gap, Length::Px(10.0));

        let (doc2, t2) = style("<div>x</div>", "div { gap: 5px 8px }");
        let d2 = t2.computed(find(&doc2, "div"));
        assert_eq!(d2.row_gap, Length::Px(5.0));
        assert_eq!(d2.column_gap, Length::Px(8.0));

        let (doc3, t3) = style("<div>x</div>", "div { column-gap: 12px }");
        let d3 = t3.computed(find(&doc3, "div"));
        assert_eq!(d3.row_gap, Length::Px(0.0));
        assert_eq!(d3.column_gap, Length::Px(12.0));
    }

    #[test]
    fn multicol_longhands_and_shorthands() {
        // longhands.
        let (doc, t) = style(
            "<div>x</div>",
            "div { column-count: 3; column-width: 200px; column-gap: 20px; \
             column-rule-width: 2px; column-rule-style: solid; column-rule-color: #f00 }",
        );
        let d = t.computed(find(&doc, "div"));
        assert_eq!(d.column_count, Some(3));
        assert_eq!(d.column_width, Some(Length::Px(200.0)));
        assert_eq!(d.column_gap, Length::Px(20.0));
        assert_eq!(d.column_rule_width, 2.0);
        assert_eq!(d.column_rule_style, BorderStyle::Solid);
        assert_eq!(d.column_rule_color, red());

        // `auto` keywords → None.
        let (doc2, t2) = style(
            "<div>x</div>",
            "div { column-count: auto; column-width: auto }",
        );
        let d2 = t2.computed(find(&doc2, "div"));
        assert_eq!(d2.column_count, None);
        assert_eq!(d2.column_width, None);

        // defaults.
        let (doc3, t3) = style("<div>x</div>", "div { color: red }");
        let d3 = t3.computed(find(&doc3, "div"));
        assert_eq!(d3.column_count, None);
        assert_eq!(d3.column_width, None);
        assert_eq!(d3.column_rule_width, 0.0);
        assert_eq!(d3.column_rule_style, BorderStyle::None);
        assert_eq!(d3.column_rule_color, black());

        // `columns` shorthand: integer → count, length → width.
        let (doc4, t4) = style("<div>x</div>", "div { columns: 3 200px }");
        let d4 = t4.computed(find(&doc4, "div"));
        assert_eq!(d4.column_count, Some(3));
        assert_eq!(d4.column_width, Some(Length::Px(200.0)));

        // `columns: auto` resets both.
        let (doc5, t5) = style(
            "<div>x</div>",
            "div { column-count: 4; column-width: 50px; columns: auto }",
        );
        let d5 = t5.computed(find(&doc5, "div"));
        assert_eq!(d5.column_count, None);
        assert_eq!(d5.column_width, None);

        // `column-rule` shorthand: width || style || color, any order.
        let (doc6, t6) = style("<div>x</div>", "div { column-rule: 2px solid #f00 }");
        let d6 = t6.computed(find(&doc6, "div"));
        assert_eq!(d6.column_rule_width, 2.0);
        assert_eq!(d6.column_rule_style, BorderStyle::Solid);
        assert_eq!(d6.column_rule_color, red());
    }

    #[test]
    fn aspect_ratio_parsing() {
        // initial → None.
        let (doc0, t0) = style("<div>x</div>", "div { color: red }");
        assert_eq!(t0.computed(find(&doc0, "div")).aspect_ratio, None);

        // `<w>/<h>` ratio.
        let (doc, t) = style("<div>x</div>", "div { aspect-ratio: 16/9 }");
        let r = t.computed(find(&doc, "div")).aspect_ratio.unwrap();
        assert!((r - 16.0 / 9.0).abs() < 1e-6);

        // single `<number>`.
        let (doc2, t2) = style("<div>x</div>", "div { aspect-ratio: 1.5 }");
        assert_eq!(t2.computed(find(&doc2, "div")).aspect_ratio, Some(1.5));

        // `auto` → None.
        let (doc3, t3) = style("<div>x</div>", "div { aspect-ratio: auto }");
        assert_eq!(t3.computed(find(&doc3, "div")).aspect_ratio, None);
    }

    // --- E2-M5: background gradient / border-radius / box-shadow / opacity ---

    fn gradient(html: &str, css: &str, tag: &str) -> LinearGradient {
        let (doc, t) = style(html, css);
        let layers = &t.computed(find(&doc, tag)).background_layers;
        match layers.first().map(|l| &l.image) {
            Some(BgImage::Gradient(g)) => g.clone(),
            other => panic!("expected one gradient layer, got {other:?}"),
        }
    }

    #[test]
    fn gradient_to_right_two_stops() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(to right, red, blue) }",
            "div",
        );
        assert_eq!(g.angle_deg, 90.0);
        assert_eq!(g.stops.len(), 2);
        assert_eq!(g.stops[0].color, red());
        assert_eq!(g.stops[1].color, blue());
    }

    #[test]
    fn gradient_angle_deg() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(45deg, #000, #fff) }",
            "div",
        );
        assert_eq!(g.angle_deg, 45.0);
        assert_eq!(g.stops.len(), 2);
    }

    #[test]
    fn gradient_default_direction_to_bottom() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(red, lime, blue) }",
            "div",
        );
        assert_eq!(g.angle_deg, 180.0);
        assert_eq!(g.stops.len(), 3);
    }

    #[test]
    fn gradient_with_rgba_and_percent_stops() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(90deg, rgba(255,0,0,0.5) 0%, blue 100%) }",
            "div",
        );
        assert_eq!(g.stops.len(), 2);
        assert_eq!(
            g.stops[0].color,
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 128
            }
        );
        assert_eq!(g.stops[0].pos, Some(0.0));
        assert_eq!(g.stops[1].pos, Some(1.0));
    }

    #[test]
    fn background_solid_no_regression() {
        let (doc, t) = style("<div>x</div>", "div { background: red }");
        let s = t.computed(find(&doc, "div"));
        assert_eq!(s.background_color, red());
        assert!(s.background_layers.is_empty());
    }

    // --- E16-M2: background layers (image/size/position/repeat) ---

    #[test]
    fn bg_image_url_strips_quotes() {
        let (doc, t) = style("<div>x</div>", "div { background-image: url(\"a.png\") }");
        let layers = &t.computed(find(&doc, "div")).background_layers;
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].image, BgImage::Url("a.png".into()));
        // Defaults.
        assert_eq!(layers[0].size, BgSize::Auto);
        assert_eq!(layers[0].repeat, BgRepeat::Repeat);
        assert_eq!(
            layers[0].position,
            (LengthPct::Percent(0.0), LengthPct::Percent(0.0))
        );
    }

    #[test]
    fn bg_multiple_layers_with_longhands() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background-image: url(a.png), linear-gradient(red, blue); \
             background-size: cover, 50% auto; \
             background-repeat: no-repeat, repeat-x; \
             background-position: 10px 20px, center }",
        );
        let l = &t.computed(find(&doc, "div")).background_layers;
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].image, BgImage::Url("a.png".into()));
        assert!(matches!(l[1].image, BgImage::Gradient(_)));
        assert_eq!(l[0].size, BgSize::Cover);
        assert_eq!(
            l[1].size,
            BgSize::Explicit(BgSizeAxis::Percent(50.0), BgSizeAxis::Auto)
        );
        assert_eq!(l[0].repeat, BgRepeat::NoRepeat);
        assert_eq!(l[1].repeat, BgRepeat::RepeatX);
        assert_eq!(l[0].position, (LengthPct::Px(10.0), LengthPct::Px(20.0)));
        assert_eq!(
            l[1].position,
            (LengthPct::Percent(50.0), LengthPct::Percent(50.0))
        );
    }

    #[test]
    fn bg_size_value_cycles_across_layers() {
        // One size value applies to all layers (`i % len`).
        let (doc, t) = style(
            "<div>x</div>",
            "div { background-image: url(a.png), url(b.png); \
             background-size: contain }",
        );
        let l = &t.computed(find(&doc, "div")).background_layers;
        assert_eq!(l.len(), 2);
        assert_eq!(l[0].size, BgSize::Contain);
        assert_eq!(l[1].size, BgSize::Contain);
    }

    #[test]
    fn bg_shorthand_gradient_is_one_layer_transparent_color() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background: linear-gradient(red, blue) }",
        );
        let s = t.computed(find(&doc, "div"));
        assert_eq!(
            s.background_color,
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0
            }
        );
        assert_eq!(s.background_layers.len(), 1);
        assert!(matches!(s.background_layers[0].image, BgImage::Gradient(_)));
    }

    #[test]
    fn border_radius_shorthand_forms() {
        let (doc, t) = style("<div>x</div>", "div { border-radius: 8px }");
        assert_eq!(t.computed(find(&doc, "div")).border_radius, [8.0; 4]);
        let (doc2, t2) = style("<div>x</div>", "div { border-radius: 1px 2px }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).border_radius,
            [1.0, 2.0, 1.0, 2.0]
        );
        let (doc3, t3) = style("<div>x</div>", "div { border-radius: 1px 2px 3px }");
        assert_eq!(
            t3.computed(find(&doc3, "div")).border_radius,
            [1.0, 2.0, 3.0, 2.0]
        );
        let (doc4, t4) = style("<div>x</div>", "div { border-radius: 1px 2px 3px 4px }");
        assert_eq!(
            t4.computed(find(&doc4, "div")).border_radius,
            [1.0, 2.0, 3.0, 4.0]
        );
    }

    #[test]
    fn box_shadow_forms() {
        let (doc, t) = style("<div>x</div>", "div { box-shadow: 2px 3px 4px 1px #000 }");
        assert_eq!(
            t.computed(find(&doc, "div")).box_shadow,
            Some(BoxShadow {
                offset_x: 2.0,
                offset_y: 3.0,
                blur: 4.0,
                spread: 1.0,
                color: black()
            })
        );
        let (doc2, t2) = style("<div>x</div>", "div { box-shadow: 2px 2px red }");
        let s = t2.computed(find(&doc2, "div")).box_shadow.unwrap();
        assert_eq!((s.blur, s.spread), (0.0, 0.0));
        assert_eq!(s.color, red());
        let (doc3, t3) = style("<div>x</div>", "div { box-shadow: none }");
        assert_eq!(t3.computed(find(&doc3, "div")).box_shadow, None);
    }

    // --- E16-M3: radial/conic gradients, text-shadow, outline ---

    #[test]
    fn radial_gradient_parses_to_radial_with_stops() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background: radial-gradient(circle at center, red, lime, blue) }",
        );
        let layers = &t.computed(find(&doc, "div")).background_layers;
        match layers.first().map(|l| &l.image) {
            Some(BgImage::Radial(g)) => {
                assert_eq!(g.stops.len(), 3);
                assert_eq!(g.stops[0].color, red());
                assert_eq!(g.stops[2].color, blue());
            }
            other => panic!("expected a radial gradient, got {other:?}"),
        }
    }

    #[test]
    fn conic_gradient_parses_with_from_deg() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background: conic-gradient(from 90deg, red, blue) }",
        );
        let layers = &t.computed(find(&doc, "div")).background_layers;
        match layers.first().map(|l| &l.image) {
            Some(BgImage::Conic(g)) => {
                assert_eq!(g.from_deg, 90.0);
                assert_eq!(g.stops.len(), 2);
                assert_eq!(g.stops[0].color, red());
            }
            other => panic!("expected a conic gradient, got {other:?}"),
        }
        // No `from` prefix → default 0deg.
        let (doc2, t2) = style(
            "<div>x</div>",
            "div { background: conic-gradient(red, blue) }",
        );
        match t2
            .computed(find(&doc2, "div"))
            .background_layers
            .first()
            .map(|l| &l.image)
        {
            Some(BgImage::Conic(g)) => assert_eq!(g.from_deg, 0.0),
            other => panic!("expected a conic gradient, got {other:?}"),
        }
    }

    #[test]
    fn text_shadow_parses_and_is_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { text-shadow: 2px 3px 4px #0000ff }",
        );
        let d = t.computed(find(&doc, "div"));
        assert_eq!(
            d.text_shadow,
            Some(TextShadow {
                offset_x: 2.0,
                offset_y: 3.0,
                blur: 4.0,
                color: blue()
            })
        );
        // Inherited: the child <p> with no text-shadow gets the parent's.
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.text_shadow, d.text_shadow);
    }

    #[test]
    fn outline_shorthand_and_longhands_parse() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { outline: 3px dashed #ff0000; outline-offset: 5px }",
        );
        let o = t.computed(find(&doc, "div")).outline;
        assert_eq!(o.width, 3.0);
        assert_eq!(o.style, BorderStyle::Dashed);
        assert_eq!(o.color, red());
        assert_eq!(o.offset, 5.0);

        // Longhands + medium keyword width.
        let (doc2, t2) = style(
            "<div>x</div>",
            "div { outline-width: medium; outline-style: solid; outline-color: #0000ff }",
        );
        let o2 = t2.computed(find(&doc2, "div")).outline;
        assert_eq!(o2.width, 3.0);
        assert_eq!(o2.style, BorderStyle::Solid);
        assert_eq!(o2.color, blue());
    }

    #[test]
    fn outline_not_inherited() {
        let (doc, t) = style("<div><p>x</p></div>", "div { outline: 2px solid #ff0000 }");
        let p = t.computed(find(&doc, "p")).outline;
        assert_eq!(p.width, 0.0);
        assert_eq!(p.style, BorderStyle::None);
    }

    #[test]
    fn opacity_clamps() {
        let (doc, t) = style("<div>x</div>", "div { opacity: 0.5 }");
        assert_eq!(t.computed(find(&doc, "div")).opacity, 0.5);
        let (doc2, t2) = style("<div>x</div>", "div { opacity: 2 }");
        assert_eq!(t2.computed(find(&doc2, "div")).opacity, 1.0);
        let (doc3, t3) = style("<div>x</div>", "div { opacity: -1 }");
        assert_eq!(t3.computed(find(&doc3, "div")).opacity, 0.0);
    }

    #[test]
    fn visual_effects_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { border-radius: 10px; opacity: 0.5; box-shadow: 1px 1px #000; \
             background: linear-gradient(red, blue) }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.border_radius, [0.0; 4]);
        assert_eq!(p.opacity, 1.0);
        assert_eq!(p.box_shadow, None);
        assert_eq!(
            p.background_color,
            Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0
            }
        );
        assert!(p.background_layers.is_empty());
    }

    #[test]
    fn flex_props_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { display: flex; flex-grow: 3; align-items: center; gap: 10px }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.flex_grow, 0.0);
        assert_eq!(p.align_items, AlignItems::Stretch);
        assert_eq!(p.row_gap, Length::Px(0.0));
        assert_ne!(p.display, Display::Flex);
    }

    // --- E5-M1: grid ---

    #[test]
    fn display_grid_and_inline_grid() {
        let (doc, t) = style("<div>x</div>", "div { display: grid }");
        assert_eq!(t.computed(find(&doc, "div")).display, Display::Grid);
        let (doc2, t2) = style("<div>x</div>", "div { display: inline-grid }");
        assert_eq!(t2.computed(find(&doc2, "div")).display, Display::InlineGrid);
    }

    #[test]
    fn grid_template_columns_px() {
        use TrackSize::*;
        let (doc, t) = style("<div>x</div>", "div { grid-template-columns: 100px 100px }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![Px(100.0), Px(100.0)]
        );
    }

    #[test]
    fn grid_template_columns_fr() {
        use TrackSize::*;
        let (doc, t) = style("<div>x</div>", "div { grid-template-columns: 1fr 1fr 1fr }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![Fr(1.0), Fr(1.0), Fr(1.0)]
        );
    }

    #[test]
    fn grid_template_columns_repeat() {
        use TrackSize::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: repeat(3, 1fr) }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![Fr(1.0), Fr(1.0), Fr(1.0)]
        );
    }

    #[test]
    fn grid_template_columns_repeat_multi() {
        use TrackSize::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: repeat(2, 100px 1fr) }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![Px(100.0), Fr(1.0), Px(100.0), Fr(1.0)]
        );
    }

    #[test]
    fn grid_template_columns_mixed() {
        use TrackSize::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: 100px 1fr auto }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![Px(100.0), Fr(1.0), Auto]
        );
    }

    #[test]
    fn grid_template_auto_fill_ignored() {
        // auto-fill (non-integer repeat count) → declaration dropped → initial [].
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: repeat(auto-fill, 100px) }",
        );
        assert!(t
            .computed(find(&doc, "div"))
            .grid_template_columns
            .is_empty());
    }

    #[test]
    fn grid_template_rows_none() {
        let (doc, t) = style("<div>x</div>", "div { grid-template-rows: none }");
        assert!(t.computed(find(&doc, "div")).grid_template_rows.is_empty());
    }

    #[test]
    fn grid_column_range() {
        use GridPlacement::*;
        let (doc, t) = style("<div>x</div>", "div { grid-column: 1 / 3 }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_column,
            GridLine {
                start: Line(1),
                end: Line(3)
            }
        );
    }

    #[test]
    fn grid_column_span() {
        use GridPlacement::*;
        let (doc, t) = style("<div>x</div>", "div { grid-column: span 2 }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_column,
            GridLine {
                start: Span(2),
                end: Auto
            }
        );
    }

    #[test]
    fn grid_row_single_and_negative() {
        use GridPlacement::*;
        let (doc, t) = style("<div>x</div>", "div { grid-row: 2 }");
        assert_eq!(
            t.computed(find(&doc, "div")).grid_row,
            GridLine {
                start: Line(2),
                end: Auto
            }
        );
        let (doc2, t2) = style("<div>x</div>", "div { grid-column: -1 }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).grid_column,
            GridLine {
                start: Line(-1),
                end: Auto
            }
        );
    }

    #[test]
    fn grid_column_longhands() {
        use GridPlacement::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-column-start: 2; grid-column-end: 4 }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_column,
            GridLine {
                start: Line(2),
                end: Line(4)
            }
        );
    }

    #[test]
    fn grid_props_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { display: grid; grid-template-columns: 1fr 1fr; grid-column: 1 / 3 }",
        );
        let p = t.computed(find(&doc, "p"));
        assert!(p.grid_template_columns.is_empty());
        assert_eq!(p.grid_column, GridLine::AUTO);
        assert_ne!(p.display, Display::Grid);
    }

    // --- E5-M2: grid alignment + named areas ---

    #[test]
    fn grid_justify_items_values() {
        let cases = [
            ("center", AlignItems::Center),
            ("start", AlignItems::FlexStart),
            ("end", AlignItems::FlexEnd),
            ("left", AlignItems::FlexStart),
            ("right", AlignItems::FlexEnd),
        ];
        for (kw, expect) in cases {
            let (doc, t) = style("<div>x</div>", &format!("div {{ justify-items: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "div")).justify_items, expect);
        }
    }

    #[test]
    fn grid_align_items_end_on_grid() {
        let (doc, t) = style("<div>x</div>", "div { align-items: end }");
        assert_eq!(
            t.computed(find(&doc, "div")).align_items,
            AlignItems::FlexEnd
        );
    }

    #[test]
    fn grid_justify_self_and_align_self() {
        let (doc, t) = style("<div>x</div>", "div { justify-self: stretch }");
        assert_eq!(
            t.computed(find(&doc, "div")).justify_self,
            AlignSelf::Stretch
        );
        let (doc2, t2) = style("<div>x</div>", "div {}");
        assert_eq!(
            t2.computed(find(&doc2, "div")).justify_self,
            AlignSelf::Auto
        );
        let (doc3, t3) = style("<div>x</div>", "div { align-self: center }");
        assert_eq!(
            t3.computed(find(&doc3, "div")).align_self,
            AlignSelf::Center
        );
    }

    #[test]
    fn grid_align_content_values() {
        let (doc, t) = style("<div>x</div>", "div { align-content: space-between }");
        assert_eq!(
            t.computed(find(&doc, "div")).align_content,
            JustifyContent::SpaceBetween
        );
        let (doc2, t2) = style("<div>x</div>", "div { align-content: center }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).align_content,
            JustifyContent::Center
        );
    }

    #[test]
    fn grid_template_areas_basic() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-areas: \"a a\" \"b c\" }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_areas,
            vec![
                vec!["a".to_string(), "a".to_string()],
                vec!["b".to_string(), "c".to_string()],
            ]
        );
    }

    #[test]
    fn grid_template_areas_dot_and_none() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-areas: \"h h\" \".\" \"f f\" }",
        );
        let c = t.computed(find(&doc, "div"));
        assert_eq!(c.grid_template_areas[1], vec![".".to_string()]);
        let (doc2, t2) = style("<div>x</div>", "div { grid-template-areas: none }");
        assert!(t2
            .computed(find(&doc2, "div"))
            .grid_template_areas
            .is_empty());
    }

    #[test]
    fn grid_area_name_form() {
        let (doc, t) = style("<div>x</div>", "div { grid-area: header }");
        let c = t.computed(find(&doc, "div"));
        assert_eq!(c.grid_area_name, Some("header".to_string()));
        assert_eq!(c.grid_row, GridLine::AUTO);
        assert_eq!(c.grid_column, GridLine::AUTO);
    }

    #[test]
    fn grid_area_four_line_form() {
        use GridPlacement::*;
        let (doc, t) = style("<div>x</div>", "div { grid-area: 1 / 2 / 3 / 4 }");
        let c = t.computed(find(&doc, "div"));
        assert_eq!(
            c.grid_row,
            GridLine {
                start: Line(1),
                end: Line(3)
            }
        );
        assert_eq!(
            c.grid_column,
            GridLine {
                start: Line(2),
                end: Line(4)
            }
        );
        assert_eq!(c.grid_area_name, None);
    }

    #[test]
    fn grid_align_props_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { display: grid; justify-items: center; \
             grid-template-areas: \"a a\"; grid-area: foo }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.justify_items, AlignItems::Stretch);
        assert_eq!(p.grid_area_name, None);
        assert!(p.grid_template_areas.is_empty());
    }

    // --- E5-M3: 2D transforms ---

    use computed::{LengthPct, TransformFn};

    fn xform(html: &str, css: &str, tag: &str) -> Vec<TransformFn> {
        let (doc, t) = style(html, css);
        t.computed(find(&doc, tag)).transform.clone()
    }

    #[test]
    fn transform_translate_two_and_axis() {
        let f = xform(
            "<div>x</div>",
            "div { transform: translate(20px, 10px) }",
            "div",
        );
        assert_eq!(
            f,
            vec![TransformFn::Translate(
                LengthPct::Px(20.0),
                LengthPct::Px(10.0)
            )]
        );
        let fx = xform("<div>x</div>", "div { transform: translateX(5px) }", "div");
        assert_eq!(
            fx,
            vec![TransformFn::Translate(
                LengthPct::Px(5.0),
                LengthPct::Px(0.0)
            )]
        );
        let fy = xform("<div>x</div>", "div { transform: translateY(5px) }", "div");
        assert_eq!(
            fy,
            vec![TransformFn::Translate(
                LengthPct::Px(0.0),
                LengthPct::Px(5.0)
            )]
        );
    }

    #[test]
    fn transform_translate_percent() {
        let f = xform(
            "<div>x</div>",
            "div { transform: translate(50%, 100%) }",
            "div",
        );
        assert_eq!(
            f,
            vec![TransformFn::Translate(
                LengthPct::Percent(50.0),
                LengthPct::Percent(100.0)
            )]
        );
    }

    #[test]
    fn transform_scale_forms() {
        assert_eq!(
            xform("<div>x</div>", "div { transform: scale(2) }", "div"),
            vec![TransformFn::Scale(2.0, 2.0)]
        );
        assert_eq!(
            xform("<div>x</div>", "div { transform: scale(2, 3) }", "div"),
            vec![TransformFn::Scale(2.0, 3.0)]
        );
        assert_eq!(
            xform("<div>x</div>", "div { transform: scaleX(2) }", "div"),
            vec![TransformFn::Scale(2.0, 1.0)]
        );
        assert_eq!(
            xform("<div>x</div>", "div { transform: scaleY(4) }", "div"),
            vec![TransformFn::Scale(1.0, 4.0)]
        );
    }

    #[test]
    fn transform_rotate_units() {
        let want = std::f32::consts::FRAC_PI_2;
        for css in [
            "div { transform: rotate(90deg) }",
            "div { transform: rotate(0.25turn) }",
            "div { transform: rotate(1.5708rad) }",
            "div { transform: rotate(100grad) }",
        ] {
            let f = xform("<div>x</div>", css, "div");
            match f.as_slice() {
                [TransformFn::Rotate(r)] => assert!((r - want).abs() < 1e-3, "{css}: {r}"),
                other => panic!("{css}: {other:?}"),
            }
        }
    }

    #[test]
    fn transform_skew() {
        let f = xform("<div>x</div>", "div { transform: skewX(10deg) }", "div");
        match f.as_slice() {
            [TransformFn::Skew(ax, ay)] => {
                assert!((ax - 10.0f32.to_radians()).abs() < 1e-4);
                assert_eq!(*ay, 0.0);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn transform_matrix() {
        let f = xform(
            "<div>x</div>",
            "div { transform: matrix(1,0,0,1,30,40) }",
            "div",
        );
        assert_eq!(
            f,
            vec![TransformFn::Matrix([1.0, 0.0, 0.0, 1.0, 30.0, 40.0])]
        );
    }

    #[test]
    fn transform_multiple_in_order() {
        let f = xform(
            "<div>x</div>",
            "div { transform: translate(10px) rotate(45deg) }",
            "div",
        );
        assert_eq!(f.len(), 2);
        assert_eq!(
            f[0],
            TransformFn::Translate(LengthPct::Px(10.0), LengthPct::Px(0.0))
        );
        match f[1] {
            TransformFn::Rotate(r) => assert!((r - 45.0f32.to_radians()).abs() < 1e-4),
            ref o => panic!("{o:?}"),
        }
    }

    #[test]
    fn transform_none_and_bad_fn() {
        let (doc, t) = style("<div>x</div>", "div { transform: none }");
        assert!(t.computed(find(&doc, "div")).transform.is_empty());
        // unknown function → skipped → empty → leaves initial (empty).
        let (doc2, t2) = style(
            "<div>x</div>",
            "div { transform: perspective(100px) foo(1) }",
        );
        assert!(t2.computed(find(&doc2, "div")).transform.is_empty());
    }

    #[test]
    fn transform_origin_forms() {
        let (doc, t) = style("<div>x</div>", "div { transform-origin: top left }");
        assert_eq!(
            t.computed(find(&doc, "div")).transform_origin,
            (LengthPct::Percent(0.0), LengthPct::Percent(0.0))
        );
        let (doc2, t2) = style("<div>x</div>", "div { transform-origin: center }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).transform_origin,
            (LengthPct::Percent(50.0), LengthPct::Percent(50.0))
        );
        let (doc3, t3) = style("<div>x</div>", "div { transform-origin: 10px 20px }");
        assert_eq!(
            t3.computed(find(&doc3, "div")).transform_origin,
            (LengthPct::Px(10.0), LengthPct::Px(20.0))
        );
        // default initial = center.
        let (doc4, t4) = style("<div>x</div>", "div { color: red }");
        assert_eq!(
            t4.computed(find(&doc4, "div")).transform_origin,
            (LengthPct::Percent(50.0), LengthPct::Percent(50.0))
        );
    }

    // --- E15-M1: object-fit / object-position / image-rendering ---

    #[test]
    fn object_fit_keywords() {
        use computed::ObjectFit;
        for (kw, want) in [
            ("fill", ObjectFit::Fill),
            ("contain", ObjectFit::Contain),
            ("cover", ObjectFit::Cover),
            ("none", ObjectFit::None),
            ("scale-down", ObjectFit::ScaleDown),
        ] {
            let css = format!("img {{ object-fit: {kw} }}");
            let (doc, t) = style("<img>", &css);
            assert_eq!(t.computed(find(&doc, "img")).object_fit, want, "{kw}");
        }
        // default initial = Fill; unknown keyword ignored → stays Fill.
        let (doc, t) = style("<img>", "img { object-fit: bogus }");
        assert_eq!(t.computed(find(&doc, "img")).object_fit, ObjectFit::Fill);
    }

    #[test]
    fn image_rendering_keywords() {
        use computed::ImageRendering;
        for (kw, want) in [
            ("auto", ImageRendering::Auto),
            ("smooth", ImageRendering::Smooth),
            ("high-quality", ImageRendering::Smooth),
            ("pixelated", ImageRendering::Pixelated),
            ("crisp-edges", ImageRendering::CrispEdges),
        ] {
            let css = format!("img {{ image-rendering: {kw} }}");
            let (doc, t) = style("<img>", &css);
            assert_eq!(t.computed(find(&doc, "img")).image_rendering, want, "{kw}");
        }
    }

    #[test]
    fn object_position_one_and_two_values() {
        // 1 value → y defaults to center (50%).
        let (doc, t) = style("<img>", "img { object-position: 10px }");
        assert_eq!(
            t.computed(find(&doc, "img")).object_position,
            (LengthPct::Px(10.0), LengthPct::Percent(50.0))
        );
        // 2 values.
        let (doc2, t2) = style("<img>", "img { object-position: left top }");
        assert_eq!(
            t2.computed(find(&doc2, "img")).object_position,
            (LengthPct::Percent(0.0), LengthPct::Percent(0.0))
        );
        // default initial = center center.
        let (doc3, t3) = style("<img>", "img { color: red }");
        assert_eq!(
            t3.computed(find(&doc3, "img")).object_position,
            (LengthPct::Percent(50.0), LengthPct::Percent(50.0))
        );
    }

    #[test]
    fn image_rendering_inherits_object_fit_does_not() {
        use computed::{ImageRendering, ObjectFit};
        let (doc, t) = style(
            "<div><img></div>",
            "div { image-rendering: pixelated; object-fit: cover }",
        );
        let img = t.computed(find(&doc, "img"));
        // image-rendering is inherited from the div.
        assert_eq!(img.image_rendering, ImageRendering::Pixelated);
        // object-fit is NOT inherited → resets to the initial Fill on the child.
        assert_eq!(img.object_fit, ObjectFit::Fill);
    }

    // --- E6-M3: direction / unicode-bidi / spacing / transform / white-space ---

    #[test]
    fn direction_rtl_and_inherits() {
        let (doc, t) = style("<div><p>x</p></div>", "div { direction: rtl }");
        assert_eq!(t.computed(find(&doc, "div")).direction, Direction::Rtl);
        // child inherits.
        assert_eq!(t.computed(find(&doc, "p")).direction, Direction::Rtl);
    }

    #[test]
    fn unicode_bidi_not_inherited() {
        let (doc, t) = style("<div><p>x</p></div>", "div { unicode-bidi: bidi-override }");
        assert_eq!(
            t.computed(find(&doc, "div")).unicode_bidi,
            UnicodeBidi::BidiOverride
        );
        // NOT inherited → child stays Normal.
        assert_eq!(
            t.computed(find(&doc, "p")).unicode_bidi,
            UnicodeBidi::Normal
        );
    }

    // --- E18-M3: writing-mode / text-orientation ---

    #[test]
    fn writing_mode_values_and_inherit() {
        for (kw, want) in [
            ("horizontal-tb", WritingMode::HorizontalTb),
            ("vertical-rl", WritingMode::VerticalRl),
            ("vertical-lr", WritingMode::VerticalLr),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ writing-mode: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).writing_mode, want);
        }
        // inherited: a child inherits the parent's writing-mode.
        let (doc, t) = style("<div><p>x</p></div>", "div { writing-mode: vertical-rl }");
        assert_eq!(
            t.computed(find(&doc, "div")).writing_mode,
            WritingMode::VerticalRl
        );
        assert_eq!(
            t.computed(find(&doc, "p")).writing_mode,
            WritingMode::VerticalRl
        );
    }

    #[test]
    fn text_orientation_values_and_inherit() {
        for (kw, want) in [
            ("mixed", TextOrientation::Mixed),
            ("upright", TextOrientation::Upright),
            ("sideways", TextOrientation::Sideways),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ text-orientation: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).text_orientation, want);
        }
        // inherited.
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { text-orientation: upright }",
        );
        assert_eq!(
            t.computed(find(&doc, "span")).text_orientation,
            TextOrientation::Upright
        );
    }

    #[test]
    fn writing_mode_initial_is_horizontal() {
        // Default page: writing-mode initial is HorizontalTb (byte-identity gate).
        let (doc, t) = style("<p>x</p>", "");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.writing_mode, WritingMode::HorizontalTb);
        assert!(!p.writing_mode.is_vertical());
        assert_eq!(p.text_orientation, TextOrientation::Mixed);
    }

    #[test]
    fn letter_word_spacing_lengths() {
        let (doc, t) = style("<p>x</p>", "p { letter-spacing: 5px; word-spacing: 3px }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.letter_spacing, 5.0);
        assert_eq!(p.word_spacing, 3.0);
        // normal → 0.
        let (doc2, t2) = style("<p>x</p>", "p { letter-spacing: normal }");
        assert_eq!(t2.computed(find(&doc2, "p")).letter_spacing, 0.0);
        // 2em against 16px font = 32.
        let (doc3, t3) = style("<p>x</p>", "p { font-size: 16px; letter-spacing: 2em }");
        assert_eq!(t3.computed(find(&doc3, "p")).letter_spacing, 32.0);
    }

    #[test]
    fn spacing_inherits() {
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { letter-spacing: 4px; word-spacing: 6px }",
        );
        let s = t.computed(find(&doc, "span"));
        assert_eq!(s.letter_spacing, 4.0);
        assert_eq!(s.word_spacing, 6.0);
    }

    #[test]
    fn text_indent_lengths_and_inherit() {
        // px / % / em against the element's own font-size.
        let (doc, t) = style("<p>x</p>", "p { text-indent: 20px }");
        assert_eq!(t.computed(find(&doc, "p")).text_indent, LengthPct::Px(20.0));
        let (doc, t) = style("<p>x</p>", "p { text-indent: 25% }");
        assert_eq!(
            t.computed(find(&doc, "p")).text_indent,
            LengthPct::Percent(25.0)
        );
        let (doc, t) = style("<p>x</p>", "p { font-size: 16px; text-indent: 2em }");
        assert_eq!(t.computed(find(&doc, "p")).text_indent, LengthPct::Px(32.0));
        // initial = 0.
        let (doc, t) = style("<p>x</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).text_indent, LengthPct::Px(0.0));
        // inherited to descendant.
        let (doc, t) = style("<div><span>x</span></div>", "div { text-indent: 12px }");
        assert_eq!(
            t.computed(find(&doc, "span")).text_indent,
            LengthPct::Px(12.0)
        );
    }

    #[test]
    fn text_justify_values_and_inherit() {
        for (kw, want) in [
            ("auto", TextJustify::Auto),
            ("inter-word", TextJustify::InterWord),
            ("none", TextJustify::None),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ text-justify: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).text_justify, want);
        }
        // initial = Auto.
        let (doc, t) = style("<p>x</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).text_justify, TextJustify::Auto);
        // inherited to descendant.
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { text-justify: inter-word }",
        );
        assert_eq!(
            t.computed(find(&doc, "span")).text_justify,
            TextJustify::InterWord
        );
    }

    #[test]
    fn text_transform_values_and_inherit() {
        for (kw, want) in [
            ("uppercase", TextTransform::Uppercase),
            ("lowercase", TextTransform::Lowercase),
            ("capitalize", TextTransform::Capitalize),
            ("none", TextTransform::None),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ text-transform: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).text_transform, want);
        }
        // inherited.
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { text-transform: uppercase }",
        );
        assert_eq!(
            t.computed(find(&doc, "span")).text_transform,
            TextTransform::Uppercase
        );
    }

    #[test]
    fn white_space_values_and_inherit() {
        for (kw, want) in [
            ("normal", WhiteSpace::Normal),
            ("pre", WhiteSpace::Pre),
            ("nowrap", WhiteSpace::Nowrap),
            ("pre-wrap", WhiteSpace::PreWrap),
            ("pre-line", WhiteSpace::PreLine),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ white-space: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).white_space, want);
        }
        // break-spaces (E22-M1).
        let (doc, t) = style("<p>x</p>", "p { white-space: break-spaces }");
        assert_eq!(
            t.computed(find(&doc, "p")).white_space,
            WhiteSpace::BreakSpaces
        );
        // inherited.
        let (doc, t) = style("<div><span>x</span></div>", "div { white-space: pre }");
        assert_eq!(t.computed(find(&doc, "span")).white_space, WhiteSpace::Pre);
    }

    #[test]
    fn word_break_values_and_inherit() {
        for (kw, want) in [
            ("normal", WordBreak::Normal),
            ("break-all", WordBreak::BreakAll),
            ("keep-all", WordBreak::KeepAll),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ word-break: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).word_break, want);
        }
        let (doc, t) = style("<div><span>x</span></div>", "div { word-break: break-all }");
        assert_eq!(
            t.computed(find(&doc, "span")).word_break,
            WordBreak::BreakAll
        );
    }

    #[test]
    fn overflow_wrap_values_alias_and_inherit() {
        for (kw, want) in [
            ("normal", OverflowWrap::Normal),
            ("break-word", OverflowWrap::BreakWord),
            ("anywhere", OverflowWrap::Anywhere),
        ] {
            let (doc, t) = style("<p>x</p>", &format!("p {{ overflow-wrap: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "p")).overflow_wrap, want);
        }
        // `word-wrap` is a legacy alias.
        let (doc, t) = style("<p>x</p>", "p { word-wrap: break-word }");
        assert_eq!(
            t.computed(find(&doc, "p")).overflow_wrap,
            OverflowWrap::BreakWord
        );
        // inherited.
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { overflow-wrap: anywhere }",
        );
        assert_eq!(
            t.computed(find(&doc, "span")).overflow_wrap,
            OverflowWrap::Anywhere
        );
    }

    #[test]
    fn tab_size_number_px_and_inherit() {
        let (doc, t) = style("<p>x</p>", "p { tab-size: 4 }");
        assert_eq!(t.computed(find(&doc, "p")).tab_size, TabSize::Number(4.0));
        let (doc, t) = style("<p>x</p>", "p { tab-size: 20px }");
        assert_eq!(t.computed(find(&doc, "p")).tab_size, TabSize::Px(20.0));
        // default is Number(8.0).
        let (doc, t) = style("<p>x</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).tab_size, TabSize::Number(8.0));
        // inherited.
        let (doc, t) = style("<div><span>x</span></div>", "div { tab-size: 2 }");
        assert_eq!(
            t.computed(find(&doc, "span")).tab_size,
            TabSize::Number(2.0)
        );
    }

    #[test]
    fn gradient_with_transparent_stop_keeps_two_stops() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background: linear-gradient(#000, transparent) }",
        );
        let s = t.computed(find(&doc, "div"));
        let g = match &s.background_layers[0].image {
            BgImage::Gradient(g) => g,
            other => panic!("expected gradient, got {other:?}"),
        };
        assert_eq!(g.stops.len(), 2);
        assert_eq!(g.stops[1].color.a, 0);
    }

    #[test]
    fn transform_not_inherited() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { transform: rotate(45deg); transform-origin: 0 0 }",
        );
        let p = t.computed(find(&doc, "p"));
        assert!(p.transform.is_empty());
        assert_eq!(
            p.transform_origin,
            (LengthPct::Percent(50.0), LengthPct::Percent(50.0))
        );
    }

    // --- E7-M2: ::before / ::after pseudo cascade ---

    #[test]
    fn pseudo_element_matches_origin_element() {
        // `div::before` matches the <div> element itself (matcher ignores the
        // pseudo-element).
        let sheet = parse_stylesheet("div::before { content: \"x\" }");
        let doc = parse("<div>x</div>");
        assert!(matches(
            &doc,
            find(&doc, "div"),
            &sheet.rules[0].selectors[0]
        ));
    }

    #[test]
    fn pseudo_before_text_entry() {
        let (doc, t) = style("<div>x</div>", "div::before { content: \"x\" }");
        let div = find(&doc, "div");
        let (_, text) = t.pseudo(div, PseudoElement::Before).expect("before entry");
        assert_eq!(text, "x");
        assert!(t.pseudo(div, PseudoElement::After).is_none());
    }

    #[test]
    fn pseudo_none_normal_no_decl_no_entry() {
        for css in [
            "div::before { content: none }",
            "div::before { content: normal }",
            "div::before { color: red }",
        ] {
            let (doc, t) = style("<div>x</div>", css);
            assert!(
                t.pseudo(find(&doc, "div"), PseudoElement::Before).is_none(),
                "{css}"
            );
        }
    }

    #[test]
    fn pseudo_empty_string_makes_entry() {
        let (doc, t) = style("<div>x</div>", "div::before { content: \"\" }");
        let (_, text) = t
            .pseudo(find(&doc, "div"), PseudoElement::Before)
            .expect("entry for empty content");
        assert_eq!(text, "");
    }

    #[test]
    fn pseudo_attr_content() {
        let (doc, t) = style(
            "<span data-label='Hi'>x</span>",
            "[data-label]::before { content: attr(data-label) }",
        );
        let (_, text) = t
            .pseudo(find(&doc, "span"), PseudoElement::Before)
            .expect("entry");
        assert_eq!(text, "Hi");
    }

    #[test]
    fn pseudo_attr_missing_empty_string() {
        let (doc, t) = style(
            "<span data-x='v'>x</span>",
            "span::before { content: attr(data-missing) }",
        );
        let (_, text) = t
            .pseudo(find(&doc, "span"), PseudoElement::Before)
            .expect("entry even when attr absent");
        assert_eq!(text, "");
    }

    #[test]
    fn pseudo_string_concat() {
        let (doc, t) = style("<div>x</div>", "div::before { content: \"a\" \"b\" }");
        let (_, text) = t.pseudo(find(&doc, "div"), PseudoElement::Before).unwrap();
        assert_eq!(text, "ab");
        let (doc2, t2) = style(
            "<span data-x='v'>y</span>",
            "span::before { content: \"[\" attr(data-x) \"]\" }",
        );
        let (_, text2) = t2
            .pseudo(find(&doc2, "span"), PseudoElement::Before)
            .unwrap();
        assert_eq!(text2, "[v]");
    }

    #[test]
    fn pseudo_inherits_and_overrides_color() {
        // inherits the element's color.
        let (doc, t) = style(
            "<div>x</div>",
            "div { color: red } div::before { content: \"x\" }",
        );
        let (s, _) = t.pseudo(find(&doc, "div"), PseudoElement::Before).unwrap();
        assert_eq!(s.color, red());
        // own rule overrides.
        let (doc2, t2) = style(
            "<div>x</div>",
            "div { color: red } div::before { content: \"x\"; color: blue }",
        );
        let (s2, _) = t2
            .pseudo(find(&doc2, "div"), PseudoElement::Before)
            .unwrap();
        assert_eq!(s2.color, blue());
    }

    #[test]
    fn pseudo_specificity_class_beats_tag() {
        let (doc, t) = style(
            "<div class='c'>x</div>",
            "div::before { content: \"a\" } .c::before { content: \"b\" }",
        );
        let (_, text) = t.pseudo(find(&doc, "div"), PseudoElement::Before).unwrap();
        assert_eq!(text, "b");
    }

    // --- E7-M3: table ---

    #[test]
    fn display_table_keywords() {
        use Display::*;
        for (kw, want) in [
            ("table", Table),
            ("inline-table", InlineTable),
            ("table-row", TableRow),
            ("table-cell", TableCell),
            ("table-row-group", TableRowGroup),
            ("table-header-group", TableRowGroup),
            ("table-footer-group", TableRowGroup),
        ] {
            let (doc, t) = style("<div>x</div>", &format!("div {{ display: {kw} }}"));
            assert_eq!(t.computed(find(&doc, "div")).display, want, "{kw}");
        }
    }

    #[test]
    fn border_spacing_one_and_two_lengths() {
        let (doc, t) = style("<div>x</div>", "div { border-spacing: 4px }");
        assert_eq!(t.computed(find(&doc, "div")).border_spacing, (4.0, 4.0));
        let (doc2, t2) = style("<div>x</div>", "div { border-spacing: 4px 8px }");
        assert_eq!(t2.computed(find(&doc2, "div")).border_spacing, (4.0, 8.0));
    }

    #[test]
    fn border_collapse_values() {
        let (doc, t) = style("<div>x</div>", "div { border-collapse: collapse }");
        assert_eq!(
            t.computed(find(&doc, "div")).border_collapse,
            BorderCollapse::Collapse
        );
        // default Separate.
        let (doc2, t2) = style("<div>x</div>", "div { color: red }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).border_collapse,
            BorderCollapse::Separate
        );
    }

    #[test]
    fn ua_table_displays_and_spacing() {
        let html = "<table><thead><tr><th>H</th></tr></thead>\
            <tbody><tr><td>x</td></tr></tbody></table>";
        let (doc, t) = style(html, "");
        let table = t.computed(find(&doc, "table"));
        assert_eq!(table.display, Display::Table);
        assert_eq!(table.border_spacing, (2.0, 2.0)); // UA 2px
        assert_eq!(t.computed(find(&doc, "tr")).display, Display::TableRow);
        assert_eq!(t.computed(find(&doc, "td")).display, Display::TableCell);
        assert_eq!(
            t.computed(find(&doc, "tbody")).display,
            Display::TableRowGroup
        );
        assert_eq!(
            t.computed(find(&doc, "thead")).display,
            Display::TableRowGroup
        );
    }

    #[test]
    fn ua_th_bold_centered() {
        let (doc, t) = style("<table><tr><th>H</th></tr></table>", "");
        let th = t.computed(find(&doc, "th"));
        assert_eq!(th.font_weight, FontWeight(700));
        assert_eq!(th.text_align, TextAlign::Center);
    }

    #[test]
    fn border_spacing_inherits_to_descendant() {
        let (doc, t) = style(
            "<table><tr><td>x</td></tr></table>",
            "table { border-spacing: 6px }",
        );
        // The table reads its own computed value.
        assert_eq!(t.computed(find(&doc, "table")).border_spacing, (6.0, 6.0));
        // And it inherits to a descendant cell.
        assert_eq!(t.computed(find(&doc, "td")).border_spacing, (6.0, 6.0));
    }

    // --- E11-M2: cascade caching (pure optimization) ---

    /// Build N `<li class='x'>` under a `<ul>`.
    fn list_items_html(n: usize) -> String {
        let mut s = String::from("<ul>");
        for _ in 0..n {
            s.push_str("<li class='x'>item</li>");
        }
        s.push_str("</ul>");
        s
    }

    /// With combinator/structural-free author CSS, the cache is ON: 100 identical
    /// `<li class='x'>` share one match result, so the full match loop runs far
    /// fewer than 100 times — yet every li still computes the same correct style.
    #[test]
    fn cascade_cache_shares_identical_list_items() {
        let html = list_items_html(100);
        let css = "li.x { color: red; font-size: 14px } ul { margin: 5px }";

        cascade::CASCADE_MATCH_CALLS.with(|c| c.set(0));
        let (doc, t) = style(&html, css);
        let calls = cascade::CASCADE_MATCH_CALLS.with(|c| c.get());

        // Sharing: well below one full match per li (the 100 li collapse to one).
        assert!(calls < 20, "expected <20 match calls, got {calls}");

        // Correctness: every li is red / 14px.
        let mut count = 0;
        let mut stack = vec![doc.root()];
        while let Some(node) = stack.pop() {
            if doc.tag_name(node) == Some("li") {
                let s = t.computed(node);
                assert_eq!(s.color, red());
                assert_eq!(s.font_size, 14.0);
                count += 1;
            }
            for c in doc.children(node) {
                stack.push(c);
            }
        }
        assert_eq!(count, 100);
    }

    /// A structural selector (`:nth-child`) disables the cache: every li runs its
    /// own full match (>= 100 calls), and the position-dependent result is still
    /// correct — even rows red, odd rows blue.
    #[test]
    fn cascade_cache_disabled_for_structural_selectors() {
        let html = list_items_html(100);
        let css = "li:nth-child(even) { color: red } li { color: blue }";

        cascade::CASCADE_MATCH_CALLS.with(|c| c.set(0));
        let (doc, t) = style(&html, css);
        let calls = cascade::CASCADE_MATCH_CALLS.with(|c| c.get());

        // No sharing: at least one full match per li.
        assert!(calls >= 100, "expected >=100 match calls, got {calls}");

        // Correctness via the original (uncached) path: 1-based even→red, odd→blue.
        let mut idx = 0;
        let mut stack = vec![doc.root()];
        // Collect li in document order.
        let mut lis = Vec::new();
        while let Some(node) = stack.pop() {
            if doc.tag_name(node) == Some("li") {
                lis.push(node);
            }
            for c in doc.children(node).into_iter().rev() {
                stack.push(c);
            }
        }
        for li in lis {
            idx += 1;
            let want = if idx % 2 == 0 { red() } else { blue() };
            assert_eq!(t.computed(li).color, want, "li #{idx}");
        }
    }

    /// A combinator in the sheet (`ul li`) also disables the cache, and the
    /// descendant match is still computed correctly.
    #[test]
    fn cascade_cache_disabled_for_combinator() {
        let (doc, t) = style("<ul><li>a</li></ul>", "ul li { color: red }");
        assert_eq!(t.computed(find(&doc, "li")).color, red());
    }

    /// Regression: an attribute referenced only INSIDE `:not(...)` must enter the
    /// cache key. `:not([hidden])` is position-independent (cache stays ON), so two
    /// otherwise-identical `<p class=x>` differing only in `hidden` must NOT share a
    /// key — else one would wrongly inherit the other's `:not([hidden])` result.
    #[test]
    fn cascade_cache_keys_on_not_attr() {
        let html = "<p class='x'>visible</p><p class='x' hidden>hidden</p>";
        let css = "p:not([hidden]) { color: red }";
        let (doc, t) = style(html, css);
        // Collect the two <p> in document order.
        let mut ps = Vec::new();
        let mut stack = vec![doc.root()];
        while let Some(node) = stack.pop() {
            if doc.tag_name(node) == Some("p") {
                ps.push(node);
            }
            for c in doc.children(node).into_iter().rev() {
                stack.push(c);
            }
        }
        assert_eq!(ps.len(), 2);
        // First <p> (no hidden) matches :not([hidden]) → red; second (hidden) does not → black.
        assert_eq!(t.computed(ps[0]).color, red(), "p without hidden → red");
        assert_eq!(t.computed(ps[1]).color, black(), "p with hidden → not red");
    }

    /// E14-M3 regression: a state pseudo-class (`:disabled`) reads the element's
    /// own `disabled` attribute, so it is position-INDEPENDENT (cache stays ON) —
    /// but two otherwise-identical `<input>` differing only in `disabled` must NOT
    /// share a cache entry, else one would inherit the other's `:disabled` result.
    #[test]
    fn cascade_cache_keys_on_disabled_state() {
        // 50 enabled + 50 disabled inputs, all otherwise identical.
        let mut html = String::new();
        for _ in 0..50 {
            html.push_str("<input class='y'>");
            html.push_str("<input class='y' disabled>");
        }
        let css = "input:disabled { color: red } input { color: blue }";

        cascade::CASCADE_MATCH_CALLS.with(|c| c.set(0));
        let (doc, t) = style(&html, css);
        let calls = cascade::CASCADE_MATCH_CALLS.with(|c| c.get());

        // Cache stays ON: the 100 inputs collapse to a handful of full matches
        // (one per distinct key — enabled vs disabled — not one per element).
        assert!(
            calls < 20,
            "expected <20 match calls (cache on), got {calls}"
        );

        // Correctness: enabled inputs are blue, disabled ones red (the author
        // `:disabled` rule wins for the disabled set). The two keys must NOT alias.
        let mut stack = vec![doc.root()];
        let mut enabled = 0;
        let mut disabled = 0;
        while let Some(n) = stack.pop() {
            if doc.tag_name(n) == Some("input") {
                if doc.get_attribute(n, "disabled").is_some() {
                    assert_eq!(t.computed(n).color, red(), "disabled input → red");
                    disabled += 1;
                } else {
                    assert_eq!(t.computed(n).color, blue(), "enabled input → blue");
                    enabled += 1;
                }
            }
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        assert_eq!((enabled, disabled), (50, 50));
    }

    // --- E16-M1: :is / :where / :has cascade + cache ---

    /// `:where()` contributes zero specificity, so a competing `.cls` rule wins.
    #[test]
    fn where_zero_specificity_loses() {
        let (doc, t) = style(
            "<div id='id' class='cls'>x</div>",
            ":where(#id) { color: red } .cls { color: blue }",
        );
        assert_eq!(t.computed(find_id(&doc, "id")).color, blue());
    }

    /// A `:has(.x)` sheet disables the cache (subtree-dependent): two divs that
    /// differ only by a `.x` child get different styles, and the full match loop
    /// runs per element.
    #[test]
    fn has_disables_cache_and_distinguishes_subtrees() {
        let html = "<div id='yes'><span class='x'>a</span></div>\
                    <div id='no'><span>b</span></div>";
        let css = "div:has(.x) { color: red } div { color: blue }";

        cascade::CASCADE_MATCH_CALLS.with(|c| c.set(0));
        let (doc, t) = style(html, css);
        let calls = cascade::CASCADE_MATCH_CALLS.with(|c| c.get());

        // Cache OFF: at least one full match per element (>= the 2 divs + spans).
        assert!(calls >= 4, "expected cache off (>=4 calls), got {calls}");
        assert_eq!(t.computed(find_id(&doc, "yes")).color, red());
        assert_eq!(t.computed(find_id(&doc, "no")).color, blue());
    }

    /// `:is(.a, .b)` with only plain compounds keeps the cache ON (low calls).
    #[test]
    fn is_plain_keeps_cache_on() {
        let html = list_items_html(100);
        // every li has class x; :is(.x, .y) matches via .x.
        let css = "li:is(.x, .y) { color: red }";

        cascade::CASCADE_MATCH_CALLS.with(|c| c.set(0));
        let (doc, t) = style(&html, css);
        let calls = cascade::CASCADE_MATCH_CALLS.with(|c| c.get());

        assert!(calls < 20, "expected cache on (<20 calls), got {calls}");
        let mut count = 0;
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.tag_name(n) == Some("li") {
                assert_eq!(t.computed(n).color, red());
                count += 1;
            }
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        assert_eq!(count, 100);
    }

    /// Regression (mirror of `cascade_cache_keys_on_not_attr`): an attribute
    /// referenced only inside `:is([data-x])` must enter the cache key, so two
    /// `<p class=x>` differing only in `data-x` do NOT share a result.
    #[test]
    fn is_attr_keys_the_cache() {
        let html = "<p class='x' data-x>has</p><p class='x'>none</p>";
        let css = "p:is([data-x]) { color: red }";
        let (doc, t) = style(html, css);
        let mut ps = Vec::new();
        let mut stack = vec![doc.root()];
        while let Some(node) = stack.pop() {
            if doc.tag_name(node) == Some("p") {
                ps.push(node);
            }
            for c in doc.children(node).into_iter().rev() {
                stack.push(c);
            }
        }
        assert_eq!(ps.len(), 2);
        assert_eq!(t.computed(ps[0]).color, red(), "p with data-x → red");
        assert_eq!(
            t.computed(ps[1]).color,
            black(),
            "p without data-x → not red"
        );
    }

    // --- E16-M1: CSS counters ---

    /// Collect the `::before` content text of every element with a given tag, in
    /// document order.
    fn before_texts(doc: &Document, t: &StyledTree, tag: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut stack = vec![doc.root()];
        // collect in document order
        let mut nodes = Vec::new();
        while let Some(n) = stack.pop() {
            nodes.push(n);
            for c in doc.children(n).into_iter().rev() {
                stack.push(c);
            }
        }
        for n in nodes {
            if doc.tag_name(n) == Some(tag) {
                if let Some((_, text)) = t.pseudo(n, PseudoElement::Before) {
                    out.push(text.clone());
                }
            }
        }
        out
    }

    #[test]
    fn counter_decimal_sequence() {
        let (doc, t) = style(
            "<ol><li>a</li><li>b</li><li>c</li></ol>",
            "ol { counter-reset: item } li { counter-increment: item } \
             li::before { content: counter(item) \". \" }",
        );
        assert_eq!(before_texts(&doc, &t, "li"), ["1. ", "2. ", "3. "]);
    }

    #[test]
    fn counter_lower_roman_style() {
        let (doc, t) = style(
            "<ol><li>a</li><li>b</li><li>c</li></ol>",
            "ol { counter-reset: item } li { counter-increment: item } \
             li::before { content: counter(item, lower-roman) }",
        );
        assert_eq!(before_texts(&doc, &t, "li"), ["i", "ii", "iii"]);
    }

    #[test]
    fn counters_nested_join() {
        // Nested ordered lists: outer item 1 contains items 1.1 / 1.2.
        let (doc, t) = style(
            "<ol><li>a<ol><li>a1</li><li>a2</li></ol></li><li>b</li></ol>",
            "ol { counter-reset: item } li { counter-increment: item } \
             li::before { content: counters(item, \".\") \" \" }",
        );
        // document order: outer li#1, inner li#1, inner li#2, outer li#2.
        assert_eq!(before_texts(&doc, &t, "li"), ["1 ", "1.1 ", "1.2 ", "2 "]);
    }

    #[test]
    fn counter_reset_scope_does_not_leak_to_siblings() {
        // A counter reset inside one subtree must be popped before the sibling,
        // but increments on the OUTER scope persist across siblings.
        let (doc, t) = style(
            "<ol><li>a</li><li>b</li></ol>",
            "ol { counter-reset: item } li { counter-increment: item } \
             li::before { content: counter(item) }",
        );
        assert_eq!(before_texts(&doc, &t, "li"), ["1", "2"]);
    }

    // --- E13-M2: calc() ---

    #[test]
    fn calc_width_percent_minus_px() {
        let (doc, t) = style("<p>x</p>", "p { width: calc(100% - 20px) }");
        assert_eq!(
            t.computed(find(&doc, "p")).width,
            Length::Calc {
                px: -20.0,
                percent: 100.0
            }
        );
    }

    #[test]
    fn calc_nested() {
        let (doc, t) = style("<p>x</p>", "p { width: calc(calc(50% - 10px) + 5px) }");
        assert_eq!(
            t.computed(find(&doc, "p")).width,
            Length::Calc {
                px: -5.0,
                percent: 50.0
            }
        );
    }

    #[test]
    fn calc_pure_percent_normalizes() {
        let (doc, t) = style("<p>x</p>", "p { width: calc(100% / 2) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Percent(50.0));
    }

    #[test]
    fn calc_em_folds_to_px() {
        // p font-size defaults to 16 → 1em = 16px.
        let (doc, t) = style("<p>x</p>", "p { margin-left: calc(10px + 1em) }");
        assert_eq!(t.computed(find(&doc, "p")).margin_left, Length::Px(26.0));
    }

    #[test]
    fn calc_font_size_percent_plus_px() {
        // parent (body) font-size 16 → calc(100% + 4px) = 20.
        let (doc, t) = style("<p>x</p>", "p { font-size: calc(100% + 4px) }");
        assert_eq!(t.computed(find(&doc, "p")).font_size, 20.0);
    }

    #[test]
    fn calc_type_error_is_noop() {
        // 50% * 50% is invalid → width left at initial Auto.
        let (doc, t) = style("<p>x</p>", "p { width: calc(50% * 50%) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Auto);
    }

    #[test]
    fn calc_div_by_zero_is_noop() {
        let (doc, t) = style("<p>x</p>", "p { width: calc(10px / 0) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Auto);
    }

    // --- E13-M2: custom properties + var() ---

    #[test]
    fn var_basic() {
        let (doc, t) = style("<p>x</p>", "p { --c: red; color: var(--c) }");
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    #[test]
    fn var_fallback_used() {
        let (doc, t) = style("<p>x</p>", "p { color: var(--missing, blue) }");
        assert_eq!(t.computed(find(&doc, "p")).color, blue());
    }

    #[test]
    fn var_unresolved_no_fallback_invalid() {
        // No `--missing`, no fallback → declaration invalid → initial black.
        let (doc, t) = style("<p>x</p>", "p { color: var(--missing) }");
        assert_eq!(t.computed(find(&doc, "p")).color, black());
    }

    #[test]
    fn var_inherited_from_parent() {
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { --c: red } p { color: var(--c) }",
        );
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    #[test]
    fn var_holding_calc() {
        let (doc, t) = style("<p>x</p>", "p { --w: calc(50% + 10px); width: var(--w) }");
        assert_eq!(
            t.computed(find(&doc, "p")).width,
            Length::Calc {
                px: 10.0,
                percent: 50.0
            }
        );
    }

    #[test]
    fn var_nested_reference() {
        let (doc, t) = style(
            "<p>x</p>",
            "p { --a: 10px; --b: var(--a); margin-left: var(--b) }",
        );
        assert_eq!(t.computed(find(&doc, "p")).margin_left, Length::Px(10.0));
    }

    #[test]
    fn var_cycle_is_noop() {
        // --a and --b reference each other; width:var(--a) → invalid, no panic.
        let (doc, t) = style(
            "<p>x</p>",
            "p { --a: var(--b); --b: var(--a); width: var(--a) }",
        );
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Auto);
    }

    #[test]
    fn custom_prop_case_sensitive() {
        // `--C` (uppercase) is a distinct property from `--c`; var(--c) misses.
        let (doc, t) = style("<p>x</p>", "p { --C: red; color: var(--c, blue) }");
        assert_eq!(t.computed(find(&doc, "p")).color, blue());
    }

    // --- E13-M3: @media queries + viewport units ---

    /// Style a page at an explicit viewport.
    fn style_vp(html: &str, css: &str, vp: Viewport) -> (Document, StyledTree) {
        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let tree = style_tree_vp(&doc, &[sheet], vp);
        (doc, tree)
    }

    #[test]
    fn media_max_width_applies_only_when_narrow() {
        let css = "p { color: black } @media (max-width:500px) { p { color: red } }";
        // vp width 400 ≤ 500 → applies.
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(400.0));
        assert_eq!(t.computed(find(&d, "p")).color, red());
        // vp width 600 > 500 → not applied (stays black).
        let (d2, t2) = style_vp("<p>x</p>", css, Viewport::from_width(600.0));
        assert_eq!(t2.computed(find(&d2, "p")).color, black());
    }

    #[test]
    fn media_min_width_applies() {
        let css = "@media (min-width:500px) { p { color: red } }";
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find(&d, "p")).color, red());
        let (d2, t2) = style_vp("<p>x</p>", css, Viewport::from_width(400.0));
        assert_eq!(t2.computed(find(&d2, "p")).color, black());
    }

    #[test]
    fn media_and_both_must_hold() {
        let css = "@media screen and (min-width:400px) and (max-width:900px) \
                   { p { color: red } }";
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find(&d, "p")).color, red());
        // width 1000 fails the max-width branch.
        let (d2, t2) = style_vp("<p>x</p>", css, Viewport::from_width(1000.0));
        assert_eq!(t2.computed(find(&d2, "p")).color, black());
    }

    #[test]
    fn media_comma_or_matches_either() {
        let css = "@media (max-width:300px), (min-width:700px) { p { color: red } }";
        // 800 ≥ 700 → second branch matches.
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find(&d, "p")).color, red());
        // 500 matches neither.
        let (d2, t2) = style_vp("<p>x</p>", css, Viewport::from_width(500.0));
        assert_eq!(t2.computed(find(&d2, "p")).color, black());
    }

    #[test]
    fn media_not_screen_never_matches_on_screen() {
        let css = "@media not screen { p { color: red } }";
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find(&d, "p")).color, black());
    }

    #[test]
    fn media_orientation_portrait() {
        // Build a portrait viewport directly (height > width).
        let css = "@media (orientation:portrait) { p { color: red } }";
        let portrait = Viewport {
            width: 400.0,
            height: 800.0,
        };
        let (d, t) = style_vp("<p>x</p>", css, portrait);
        assert_eq!(t.computed(find(&d, "p")).color, red());
        // landscape (4:3 default) → no match.
        let (d2, t2) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t2.computed(find(&d2, "p")).color, black());
    }

    #[test]
    fn media_source_order_interleave_green_wins() {
        // p{red}, then @media(match){p{blue}}, then p{green}: the green is LAST
        // in true source order so it wins (the media block sorts at its
        // source_index, BEFORE the later top-level green rule).
        let css = "p { color: red } \
                   @media (min-width:100px) { p { color: blue } } \
                   p { color: green }";
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find(&d, "p")).color, green());
    }

    #[test]
    fn viewport_units_width_height() {
        // width:50vw at vp 800 → Px(400); height:50vh at vp 800×600 → Px(300).
        let (d, t) = style_vp(
            "<p>x</p>",
            "p { width: 50vw; height: 50vh }",
            Viewport::from_width(800.0),
        );
        let p = t.computed(find(&d, "p"));
        assert_eq!(p.width, Length::Px(400.0));
        assert_eq!(p.height, Length::Px(300.0));
    }

    #[test]
    fn viewport_units_vmin_vmax() {
        // vp 800×600: 10vmin = 60, 10vmax = 80.
        let (d, t) = style_vp(
            "<p>x</p>",
            "p { width: 10vmin; height: 10vmax }",
            Viewport::from_width(800.0),
        );
        let p = t.computed(find(&d, "p"));
        assert_eq!(p.width, Length::Px(60.0));
        assert_eq!(p.height, Length::Px(80.0));
    }

    #[test]
    fn viewport_unit_in_calc() {
        // calc(50vw - 10px) at vp 800 → Px(390).
        let (d, t) = style_vp(
            "<p>x</p>",
            "p { width: calc(50vw - 10px) }",
            Viewport::from_width(800.0),
        );
        assert_eq!(t.computed(find(&d, "p")).width, Length::Px(390.0));
    }

    #[test]
    fn font_size_vw() {
        // font-size:5vw at vp 800 → 40px.
        let (d, t) = style_vp(
            "<p>x</p>",
            "p { font-size: 5vw }",
            Viewport::from_width(800.0),
        );
        assert_eq!(t.computed(find(&d, "p")).font_size, 40.0);
    }

    // --- E17-M1: timing + interpolation ---

    /// Style a page, then run the animation pass at clock `t` (seconds).
    fn style_at(html: &str, css: &str, t: f32) -> (Document, StyledTree) {
        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let vp = Viewport::from_width(800.0);
        let mut tree = style_tree_vp(&doc, std::slice::from_ref(&sheet), vp);
        apply_animations(&doc, std::slice::from_ref(&sheet), &mut tree, t, vp);
        (doc, tree)
    }

    #[test]
    fn easing_eval_table() {
        // Linear is the identity.
        assert_eq!(Easing::Linear.eval(0.3), 0.3);
        // ease endpoints.
        let ease = Easing::CubicBezier(0.25, 0.1, 0.25, 1.0);
        assert_eq!(ease.eval(0.0), 0.0);
        assert_eq!(ease.eval(1.0), 1.0);
        // steps(2, end): t=0.4 → 0, t=0.6 → 0.5.
        let s = Easing::Steps(2, JumpTerm::End);
        assert_eq!(s.eval(0.4), 0.0);
        assert_eq!(s.eval(0.6), 0.5);
        // cubic-bezier monotonic over a sweep.
        let mut prev = 0.0;
        for i in 0..=10 {
            let v = ease.eval(i as f32 / 10.0);
            assert!(v >= prev - 1e-4, "non-monotonic at {i}: {v} < {prev}");
            prev = v;
        }
    }

    #[test]
    fn cubic_bezier_solver_matches_reference() {
        // `ease` at t=0.5 resolves to ~0.802 (verified against a bisection
        // ground truth); the Newton/bisection solver must land close.
        let ease = Easing::CubicBezier(0.25, 0.1, 0.25, 1.0);
        assert!((ease.eval(0.5) - 0.8024).abs() < 1e-2, "{}", ease.eval(0.5));
        // ease-in at t=0.5 < linear 0.5 (slow start).
        let ein = Easing::CubicBezier(0.42, 0.0, 1.0, 1.0);
        assert!(ein.eval(0.5) < 0.5, "{}", ein.eval(0.5));
    }

    #[test]
    fn animation_opacity_fade() {
        let css = "div { animation: fade 10s linear } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } }";
        // t=0 → 0 (p = ease(0) = 0).
        let (d0, t0) = style_at("<div>x</div>", css, 0.0);
        assert_eq!(t0.computed(find(&d0, "div")).opacity, 0.0);
        // t=5 → 0.5.
        let (d5, t5) = style_at("<div>x</div>", css, 5.0);
        assert!((t5.computed(find(&d5, "div")).opacity - 0.5).abs() < 1e-3);
        // t=10 → 1.
        let (d10, t10) = style_at("<div>x</div>", css, 10.0);
        assert_eq!(t10.computed(find(&d10, "div")).opacity, 1.0);
    }

    #[test]
    fn animation_color_midpoint() {
        let css = "div { animation: c 10s linear } \
                   @keyframes c { from { color: #000000 } to { color: #ffffff } }";
        let (d, t) = style_at("<div>x</div>", css, 5.0);
        let c = t.computed(find(&d, "div")).color;
        assert_eq!((c.r, c.g, c.b), (128, 128, 128));
    }

    #[test]
    fn animation_width_length_midpoint() {
        let css = "div { animation: grow 10s linear } \
                   @keyframes grow { from { width: 100px } to { width: 200px } }";
        let (d, t) = style_at("<div>x</div>", css, 5.0);
        assert_eq!(t.computed(find(&d, "div")).width, Length::Px(150.0));
    }

    #[test]
    fn animation_no_keyframes_match_untouched() {
        // animation references a missing @keyframes → left at initial.
        let (d, t) = style_at("<div>x</div>", "div { animation: gone 10s linear }", 5.0);
        assert_eq!(t.computed(find(&d, "div")).opacity, 1.0);
    }

    #[test]
    fn animation_longhands_compose() {
        let css = "div { animation-name: fade; animation-duration: 4s; \
                   animation-timing-function: linear } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 1.0);
        // 1/4 of the way through, linear → 0.25.
        assert!((t.computed(find(&d, "div")).opacity - 0.25).abs() < 1e-3);
    }

    // --- E17-M2: full timing + transform interpolation ---

    #[test]
    fn animation_delay_shifts_start() {
        // Negative delay advances the start: at t=0 the anim is already 2s in
        // (2/10 = 0.2 progress).
        let css = "div { animation: fade 10s linear -2s } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 0.0);
        assert!((t.computed(find(&d, "div")).opacity - 0.2).abs() < 1e-3);
    }

    #[test]
    fn animation_infinite_samples_mid_iteration() {
        // infinite iteration count, large clock → wraps into a mid-iteration
        // sample. At t=25s, 10s linear from→to: raw=2.5, iter=2, local=0.5.
        let css = "div { animation: fade 10s linear infinite } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 25.0);
        assert!((t.computed(find(&d, "div")).opacity - 0.5).abs() < 1e-3);
    }

    #[test]
    fn animation_alternate_odd_iteration_reverses() {
        // alternate: odd iteration runs backwards. At t=15s (10s, 2 iters):
        // raw=1.5, iter=1 (odd) → local 0.5 reversed → 0.5 (symmetric), so use
        // t=12s: raw=1.2, iter=1 odd → 1-0.2 = 0.8.
        let css = "div { animation: fade 10s linear 2 alternate } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 12.0);
        assert!(
            (t.computed(find(&d, "div")).opacity - 0.8).abs() < 1e-3,
            "{}",
            t.computed(find(&d, "div")).opacity
        );
    }

    #[test]
    fn animation_fill_none_pre_start_uses_base() {
        // fill-mode none (default), positive delay: before start the base value
        // wins (initial opacity 1.0, NOT the 0% frame).
        let css = "div { animation: fade 10s linear 5s } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 1.0);
        assert_eq!(t.computed(find(&d, "div")).opacity, 1.0);
    }

    #[test]
    fn animation_fill_backwards_pre_start_holds_first_frame() {
        // fill backwards: before start, holds the 0% frame.
        let css = "div { animation: fade 10s linear 5s backwards } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 1.0);
        assert_eq!(t.computed(find(&d, "div")).opacity, 0.0);
    }

    #[test]
    fn animation_fill_forwards_post_end_holds_last_frame() {
        // fill forwards: after end, holds the 100% frame.
        let css = "div { animation: fade 10s linear 1 forwards } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 50.0);
        assert_eq!(t.computed(find(&d, "div")).opacity, 1.0);
    }

    #[test]
    fn animation_fill_none_post_end_uses_base() {
        // fill none: well after the end, the base value wins again.
        let css = "div { animation: fade 10s linear 1 } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 50.0);
        assert_eq!(t.computed(find(&d, "div")).opacity, 1.0);
    }

    #[test]
    fn animation_three_stops_mid_sample() {
        // 0%/50%/100% offsets; at t=2.5s of a 10s anim, p=0.25, which is the
        // midpoint of the 0%→50% span → opacity 0.25.
        let css = "div { animation: fade 10s linear } \
                   @keyframes fade { 0% { opacity: 0 } 50% { opacity: 0.5 } 100% { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 2.5);
        assert!((t.computed(find(&d, "div")).opacity - 0.25).abs() < 1e-3);
    }

    #[test]
    fn animation_transform_midpoint() {
        // translate interpolates componentwise at the midpoint.
        let css = "div { animation: slide 10s linear } \
                   @keyframes slide { from { transform: translateX(0px) } \
                                      to { transform: translateX(100px) } }";
        let (d, t) = style_at("<div>x</div>", css, 5.0);
        match t.computed(find(&d, "div")).transform.as_slice() {
            [TransformFn::Translate(x, _)] => assert_eq!(*x, LengthPct::Px(50.0)),
            other => panic!("expected one Translate, got {other:?}"),
        }
    }

    // --- E17-M3: transitions + broadened animatable properties ---

    #[test]
    fn transition_shorthand_parses() {
        let (d, t) = style("<div>x</div>", "div { transition: width 0.3s ease 0.1s }");
        let trs = &t.computed(find(&d, "div")).transitions;
        assert_eq!(trs.len(), 1);
        assert_eq!(trs[0].property, TransitionProp::Name("width".into()));
        assert!((trs[0].duration_s - 0.3).abs() < 1e-6);
        assert_eq!(trs[0].timing, Easing::CubicBezier(0.25, 0.1, 0.25, 1.0));
        assert!((trs[0].delay_s - 0.1).abs() < 1e-6);
    }

    #[test]
    fn transition_shorthand_comma_list() {
        let (d, t) = style(
            "<div>x</div>",
            "div { transition: width 1s, color 2s linear }",
        );
        let trs = &t.computed(find(&d, "div")).transitions;
        assert_eq!(trs.len(), 2);
        assert_eq!(trs[0].property, TransitionProp::Name("width".into()));
        assert!((trs[0].duration_s - 1.0).abs() < 1e-6);
        assert_eq!(trs[1].property, TransitionProp::Name("color".into()));
        assert_eq!(trs[1].timing, Easing::Linear);
    }

    #[test]
    fn transition_longhands_index_match() {
        // Two properties, one shared duration: the duration repeats to both.
        let (d, t) = style(
            "<div>x</div>",
            "div { transition-property: width, height; transition-duration: 2s }",
        );
        let trs = &t.computed(find(&d, "div")).transitions;
        assert_eq!(trs.len(), 2);
        assert_eq!(trs[0].property, TransitionProp::Name("width".into()));
        assert_eq!(trs[1].property, TransitionProp::Name("height".into()));
        assert!((trs[0].duration_s - 2.0).abs() < 1e-6);
        assert!((trs[1].duration_s - 2.0).abs() < 1e-6);
    }

    #[test]
    fn transition_property_all_and_none() {
        let (d, t) = style("<div>x</div>", "div { transition: all 1s }");
        assert_eq!(
            t.computed(find(&d, "div")).transitions[0].property,
            TransitionProp::All
        );
        let (d2, t2) = style(
            "<div>x</div>",
            "div { transition: width 1s; transition-property: none }",
        );
        assert!(t2.computed(find(&d2, "div")).transitions.is_empty());
    }

    #[test]
    fn keyframes_border_color_midpoint() {
        let css = "div { animation: bc 10s linear } \
                   @keyframes bc { from { border-color: #000000 } to { border-color: #ffffff } }";
        let (d, t) = style_at("<div>x</div>", css, 5.0);
        let c = t.computed(find(&d, "div")).border_color;
        assert_eq!((c.r, c.g, c.b), (128, 128, 128));
    }

    #[test]
    fn keyframes_border_radius_midpoint() {
        let css = "div { animation: r 10s linear } \
                   @keyframes r { from { border-radius: 0 } to { border-radius: 20px } }";
        let (d, t) = style_at("<div>x</div>", css, 5.0);
        assert_eq!(
            t.computed(find(&d, "div")).border_radius,
            [10.0, 10.0, 10.0, 10.0]
        );
    }

    #[test]
    fn keyframes_box_shadow_midpoint() {
        let css = "div { animation: s 10s linear } \
                   @keyframes s { from { box-shadow: 0 0 0 #000000 } \
                                  to { box-shadow: 10px 20px 4px #000000 } }";
        let (d, t) = style_at("<div>x</div>", css, 5.0);
        let s = t.computed(find(&d, "div")).box_shadow.unwrap();
        assert_eq!((s.offset_x, s.offset_y, s.blur), (5.0, 10.0, 2.0));
    }

    /// Style a page, mutate the styled tree's `to` value directly, then run the
    /// transition pass with a `from` cloned before the mutation.
    #[test]
    fn transition_samples_from_to_at_clock() {
        let css = "div { width: 100px; transition: width 10s linear }";
        let doc = parse("<div>x</div>");
        let sheet = parse_stylesheet(css);
        let vp = Viewport::from_width(800.0);
        let from_tree = style_tree_vp(&doc, std::slice::from_ref(&sheet), vp);
        // Build the `to` tree and bump width to 200px (as a script would).
        let mut to_tree = style_tree_vp(&doc, std::slice::from_ref(&sheet), vp);
        let id = find(&doc, "div");
        to_tree.styles.get_mut(&id).unwrap().width = Length::Px(200.0);

        // p=0 → from (100), p=0.5 → 150, p=1 → to (200).
        let mut t0 = clone_tree(&to_tree);
        apply_transitions(&doc, &from_tree, &mut t0, 0.0, vp);
        assert_eq!(t0.computed(id).width, Length::Px(100.0));

        let mut t5 = clone_tree(&to_tree);
        apply_transitions(&doc, &from_tree, &mut t5, 5.0, vp);
        assert_eq!(t5.computed(id).width, Length::Px(150.0));

        let mut t10 = clone_tree(&to_tree);
        apply_transitions(&doc, &from_tree, &mut t10, 10.0, vp);
        assert_eq!(t10.computed(id).width, Length::Px(200.0));
    }

    /// Helper: deep-clone a StyledTree for repeated sampling.
    fn clone_tree(src: &StyledTree) -> StyledTree {
        let mut out = StyledTree::default();
        for (k, v) in &src.styles {
            out.styles.insert(*k, v.clone());
        }
        out
    }

    // --- E21-M1: CSS filter ---

    use computed::FilterFn;

    fn filt(html: &str, css: &str, tag: &str) -> Vec<FilterFn> {
        let (doc, t) = style(html, css);
        t.computed(find(&doc, tag)).filter.clone()
    }

    #[test]
    fn filter_none_is_empty() {
        assert!(filt("<div>x</div>", "div { filter: none }", "div").is_empty());
    }

    #[test]
    fn filter_blur_px() {
        assert_eq!(
            filt("<div>x</div>", "div { filter: blur(4px) }", "div"),
            vec![FilterFn::Blur(4.0)]
        );
    }

    #[test]
    fn filter_grayscale_percent() {
        assert_eq!(
            filt("<div>x</div>", "div { filter: grayscale(50%) }", "div"),
            vec![FilterFn::Grayscale(0.5)]
        );
    }

    #[test]
    fn filter_brightness_number() {
        assert_eq!(
            filt("<div>x</div>", "div { filter: brightness(1.2) }", "div"),
            vec![FilterFn::Brightness(1.2)]
        );
    }

    #[test]
    fn filter_hue_rotate_deg() {
        let f = filt("<div>x</div>", "div { filter: hue-rotate(90deg) }", "div");
        match f.as_slice() {
            [FilterFn::HueRotate(rad)] => {
                assert!((rad - std::f32::consts::FRAC_PI_2).abs() < 1e-4, "{rad}");
            }
            _ => panic!("expected one HueRotate: {f:?}"),
        }
    }

    #[test]
    fn filter_drop_shadow() {
        let f = filt(
            "<div>x</div>",
            "div { filter: drop-shadow(2px 2px 3px red) }",
            "div",
        );
        assert_eq!(
            f,
            vec![FilterFn::DropShadow {
                dx: 2.0,
                dy: 2.0,
                blur: 3.0,
                color: Rgba {
                    r: 255,
                    g: 0,
                    b: 0,
                    a: 255
                },
            }]
        );
    }

    #[test]
    fn filter_chain_of_two() {
        assert_eq!(
            filt(
                "<div>x</div>",
                "div { filter: grayscale(1) brightness(0.5) }",
                "div"
            ),
            vec![FilterFn::Grayscale(1.0), FilterFn::Brightness(0.5)]
        );
    }

    #[test]
    fn filter_not_inherited() {
        let f = filt(
            "<div><span>x</span></div>",
            "div { filter: blur(4px) }",
            "span",
        );
        assert!(f.is_empty(), "filter must not inherit: {f:?}");
    }

    // --- E21-M2: blend modes ---

    use computed::BlendMode;

    #[test]
    fn mix_blend_mode_multiply() {
        let (doc, t) = style("<div>x</div>", "div { mix-blend-mode: multiply }");
        assert_eq!(
            t.computed(find(&doc, "div")).mix_blend_mode,
            BlendMode::Multiply
        );
    }

    #[test]
    fn mix_blend_mode_initial_normal() {
        let (doc, t) = style("<div>x</div>", "div {}");
        assert_eq!(
            t.computed(find(&doc, "div")).mix_blend_mode,
            BlendMode::Normal
        );
    }

    #[test]
    fn background_blend_mode_list() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background-blend-mode: multiply, screen }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).background_blend_mode,
            vec![BlendMode::Multiply, BlendMode::Screen]
        );
    }

    #[test]
    fn mix_blend_mode_not_inherited() {
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { mix-blend-mode: multiply }",
        );
        assert_eq!(
            t.computed(find(&doc, "span")).mix_blend_mode,
            BlendMode::Normal
        );
    }

    #[test]
    fn background_blend_mode_not_inherited() {
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { background-blend-mode: multiply }",
        );
        assert!(t
            .computed(find(&doc, "span"))
            .background_blend_mode
            .is_empty());
    }

    // --- E21-M3: mask-image + backdrop-filter ---

    use computed::{MaskImage, MaskMode};

    #[test]
    fn mask_image_gradient_parses() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask-image: linear-gradient(black, rgba(0,0,0,0)) }",
        );
        let m = t.computed(find(&doc, "div")).mask.clone().expect("mask");
        assert!(matches!(m.image, MaskImage::Gradient(_)));
        assert_eq!(m.mode, MaskMode::Alpha); // initial
    }

    #[test]
    fn mask_mode_luminance_parses() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask-image: linear-gradient(black, white); mask-mode: luminance }",
        );
        let m = t.computed(find(&doc, "div")).mask.clone().expect("mask");
        assert_eq!(m.mode, MaskMode::Luminance);
    }

    #[test]
    fn mask_none_clears() {
        let (doc, t) = style("<div>x</div>", "div { mask-image: none }");
        assert!(t.computed(find(&doc, "div")).mask.is_none());
    }

    #[test]
    fn mask_not_inherited() {
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { mask-image: linear-gradient(black, rgba(0,0,0,0)) }",
        );
        assert!(t.computed(find(&doc, "span")).mask.is_none());
    }

    #[test]
    fn backdrop_filter_parses() {
        let (doc, t) = style("<div>x</div>", "div { backdrop-filter: blur(3px) }");
        assert_eq!(
            t.computed(find(&doc, "div")).backdrop_filter,
            vec![FilterFn::Blur(3.0)]
        );
    }

    #[test]
    fn backdrop_filter_not_inherited() {
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { backdrop-filter: blur(3px) }",
        );
        assert!(t.computed(find(&doc, "span")).backdrop_filter.is_empty());
    }
}
