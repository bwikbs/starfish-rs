//! starfish-style — style resolution (M3).
//!
//! Given a [`Document`] and parsed author [`Stylesheet`]s, produce a
//! [`StyledTree`]: one typed [`ComputedStyle`] per element, after selector
//! matching, the cascade, and inheritance. See `docs/design/M3-style.md`.

mod calc;
mod cascade;
mod computed;
mod container;
mod counters;
mod interpolate;
mod matching;
mod media;
mod properties;
mod supports;
mod ua;

use std::collections::HashMap;

use starfish_css::{ContainerBlock, Declaration, KeyframesRule, Rule, Stylesheet};
use starfish_dom::Document;

pub use calc::MathExpr;
// E42-M3: custom `@counter-style` data, consumed by the layout marker formatter.
pub use counters::{CounterStyleData, CounterSystem};
pub use computed::{
    AlignItems, AlignSelf, AnimDirection, AnimFillMode, Animation, BackgroundLayer, BgAttachment,
    BgGeometryBox,
    BgImage,
    BgRepeat, BgSize, BgSizeAxis, BlendMode, BorderCollapse, BorderStyle, BoxShadow, BoxSizing,
    CaptionSide, Clear, ClipRadius, ClipShape, ComputedStyle, ConicGradient, ContainerType, Content,
    ContentVisibility, Direction, Display, Easing, EmphasisMark, EmphasisShape,
    FilterFn, FlexDirection, FlexWrap, Float, FontKerning, FontStyle, FontVariantCaps,
    FontVariantLigatures, FontVariantNumeric, FontWeight, GradientStop, GradientStopPos,
    AutoRepeatKind, GridAutoRepeat, GridLine,
    GridPlacement, Hyphens, ImageRendering, IndividualTransform, Isolation, JumpTerm,
    JustifyContent, Length, LengthPct,
    LineHeight, LinearGradient, ListStylePosition, ListStyleType, MaskGeometryBox, MaskImage,
    MaskMode, MaskSpec, MinMaxSize, ObjectFit, Outline, Overflow, OverflowWrap, PointerEvents, Position,
    RadialGradient,
    ScrollbarGutter, ScrollbarWidth, TabSize, TableLayout, TextAlign,
    TextDecorationLine, TextDecorationStyle, TextJustify, TextOrientation, TextOverflow, TextShadow,
    TextTransform,
    TrackSize, TransformFn, Transition, TransitionProp, UnicodeBidi, WhiteSpace, WordBreak,
    WritingMode,
};
pub use matching::matches;
pub use matching::{validity, Validity}; // E58-M3
pub use media::media_matches;
pub use starfish_css::{ColorScheme, Contrast, PointerKind, PseudoElement, Rgba};
pub use starfish_dom::NodeId;

use cascade::{cascade, cascade_pseudo, CascadeCache, ContainerEnv, Origin};
use properties::EmContext;

/// E33-M3: one cascade scope's active rules — UA + author sheets pre-flattened
/// to their viewport-active rules, each tagged with origin + layer rank.
type ActiveSheets<'a> = Vec<(Origin, Vec<(&'a Rule, u32)>)>;
/// E33-M3: per-shadow-root scoped active rules, keyed by the shadow root id.
type ScopedActive<'a> = HashMap<NodeId, ActiveSheets<'a>>;

/// Layer rank for rules outside any `@layer` (E24-M2): unlayered styles beat
/// every layered style (for normal declarations), so they get the largest rank.
/// Layered rules rank by their layer's position in the sheet's `layer_order`
/// (later-declared = larger = wins among normal declarations).
pub(crate) const UNLAYERED: u32 = u32::MAX;

/// The render viewport, in CSS px (E13-M3). Threaded into the cascade so
/// `@media` queries and `vw`/`vh`/`vmin`/`vmax` units can resolve against it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Viewport {
    pub width: f32,
    pub height: f32,
    /// Nearest query container's content-box size, for `cq*` units (E25-M1).
    /// `(0, 0)` when no query container is in scope (so `cq*` resolves to 0).
    /// `container_inline` is also the basis `cqw`/`cqi` share (horizontal-tb
    /// mapping for the MVP); `container_block` backs `cqh`/`cqb`.
    pub container_inline: f32,
    pub container_block: f32,
    /// User-preference + interaction features for `@media` (E27-M2).
    pub prefs: MediaPrefs,
}

/// The user-preference + interaction state `@media` queries read (E27-M2).
/// Defaults model a typical desktop: light scheme, no motion/contrast
/// preference, a fine pointer that can hover.
#[derive(Debug, Clone, Copy)]
pub struct MediaPrefs {
    pub color_scheme: ColorScheme,
    pub reduced_motion: bool,
    pub contrast: Contrast,
    pub pointer: PointerKind,
    pub hover: bool,
    /// Device pixel ratio in dppx, for `resolution` queries (E27-M3).
    pub dpr: f32,
}

impl Default for MediaPrefs {
    fn default() -> Self {
        MediaPrefs {
            color_scheme: ColorScheme::Light,
            reduced_motion: false,
            contrast: Contrast::NoPreference,
            pointer: PointerKind::Fine,
            hover: true,
            dpr: 1.0,
        }
    }
}

impl Viewport {
    /// Build a viewport from a width, assuming a deterministic 4:3 aspect ratio
    /// (height = width × 0.75; 800 → 600). Layout sizes the page off the width;
    /// this gives `vh`/orientation a stable height without a real layout pass.
    pub fn from_width(width: f32) -> Viewport {
        Viewport {
            width,
            height: width * 0.75,
            container_inline: 0.0,
            container_block: 0.0,
            prefs: MediaPrefs::default(),
        }
    }

    /// Copy of this viewport with the query-container size set (E25-M1).
    pub fn with_container(self, inline: f32, block: f32) -> Viewport {
        Viewport {
            container_inline: inline,
            container_block: block,
            ..self
        }
    }
}

/// Side table mapping each styled element to its computed style.
#[derive(Debug, Default)]
pub struct StyledTree {
    styles: HashMap<NodeId, ComputedStyle>,
    // E53-M3: the pseudo `(style, text)` entries are boxed. `cascade_pseudo`
    // returns a `Box` so its large by-value result occupies only an 8-byte slot in
    // the recursive `style_node`'s stack frame (one frame per nesting level); a
    // by-value `(ComputedStyle, String)` per pseudo × 5 pseudos inflated that frame
    // ~6.7 KB, overflowing the deeply-nested-table test's stack. Heap storage keeps
    // the frame small without copying on insert.
    /// Generated `::before` pseudo: element → (pseudo style, content text) (E7-M2).
    before: HashMap<NodeId, Box<(ComputedStyle, String)>>,
    /// Generated `::after` pseudo.
    after: HashMap<NodeId, Box<(ComputedStyle, String)>>,
    /// `::marker` pseudo: element → (pseudo style, content text) (E35-M1). The
    /// content string is empty when no `content` was specified (style-only rule).
    marker: HashMap<NodeId, Box<(ComputedStyle, String)>>,
    /// `::placeholder` pseudo: element → (pseudo style, content text) (E35-M2).
    /// Like `::marker`, the content string is empty for style-only rules.
    placeholder: HashMap<NodeId, Box<(ComputedStyle, String)>>,
    /// `::first-letter` pseudo: element → (pseudo style, content text) (E35-M3).
    /// Like `::marker`, the content string is empty (the styled letter comes from
    /// the block's first text run, split out in the box tree).
    first_letter: HashMap<NodeId, Box<(ComputedStyle, String)>>,
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
        // E53-M3: entries are boxed; deref to the public `&(ComputedStyle, String)`.
        let boxed = match side {
            PseudoElement::Before => self.before.get(&id),
            PseudoElement::After => self.after.get(&id),
            // E35-M1: `::marker` pseudo style for a list item.
            PseudoElement::Marker => self.marker.get(&id),
            // E35-M2: `::placeholder` pseudo style for a form control.
            PseudoElement::Placeholder => self.placeholder.get(&id),
            // E35-M3: `::first-letter` pseudo style for a block's first letter.
            PseudoElement::FirstLetter => self.first_letter.get(&id),
            // E33-M3: `::slotted` is not a generated-content pseudo.
            PseudoElement::Slotted(_) => None,
        };
        boxed.map(|b| &**b)
    }

    /// The pseudo style alone (for paint/layout style resolution via BoxStyleRef).
    pub fn pseudo_style(&self, id: NodeId, side: PseudoElement) -> Option<&ComputedStyle> {
        self.pseudo(id, side).map(|(s, _)| s)
    }

    /// True if any styled element declares a CSS transition (E17-M3). Gates the
    /// transition sampling pass so non-transition pages skip the second cascade.
    pub fn has_transitions(&self) -> bool {
        // E59-M3: a transition declared on a `::before`/`::after` pseudo also
        // requires the sampling pass.
        self.styles.values().any(|s| !s.transitions.is_empty())
            || self
                .before
                .values()
                .chain(self.after.values())
                .any(|e| !e.0.transitions.is_empty())
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
    style_tree_impl(doc, author_sheets, vp, None)
}

/// Container-aware second pass (E25-M1): same as [`style_tree_vp`] but each
/// element's `@container` rules and `cq*` units resolve against the measured
/// content-box sizes in `sizes` (keyed by the container element's `NodeId`).
/// Only called when the first pass found query containers; otherwise the
/// regular path is byte-identical.
pub fn style_tree_containers(
    doc: &Document,
    author_sheets: &[Stylesheet],
    vp: Viewport,
    sizes: &HashMap<NodeId, (f32, f32)>,
) -> StyledTree {
    style_tree_impl(doc, author_sheets, vp, Some(sizes))
}

fn style_tree_impl(
    doc: &Document,
    author_sheets: &[Stylesheet],
    vp: Viewport,
    sizes: Option<&HashMap<NodeId, (f32, f32)>>,
) -> StyledTree {
    let ua = ua::ua_stylesheet();
    // Precedence-base order: UA first, then author sheets in given order. Each
    // sheet is pre-flattened to its viewport-active rules (top-level rules with
    // matching @media blocks interleaved at their source_index) ONCE here, so
    // every per-element cascade and the match cache see the same rule sequence.
    let mut active: Vec<(Origin, Vec<(&Rule, u32)>)> =
        vec![(Origin::UserAgent, active_rules(&ua, vp))];
    for s in author_sheets {
        active.push((Origin::Author, active_rules(s, vp)));
    }

    // E25-M1: gather every `@container` block (UA + author) in precedence order.
    // Empty on pages without `@container`, so the per-element loop stays on the
    // byte-identical path. Only consulted when `sizes` is `Some` (second pass).
    let mut container_blocks: Vec<(Origin, &ContainerBlock)> = Vec::new();
    for cb in &ua.container_blocks {
        container_blocks.push((Origin::UserAgent, cb));
    }
    for s in author_sheets {
        for cb in &s.container_blocks {
            container_blocks.push((Origin::Author, cb));
        }
    }

    // E62-M1: gather every `@scope` block (UA + author) in precedence order.
    // Empty on pages without `@scope`, so the per-element cascade stays on the
    // byte-identical path. Unlike `@container`, scope membership is independent
    // of measured sizes, so these apply on both passes.
    let mut scope_blocks: Vec<(Origin, &starfish_css::ScopeBlock)> = Vec::new();
    for sb in &ua.scope_blocks {
        scope_blocks.push((Origin::UserAgent, sb));
    }
    for s in author_sheets {
        for sb in &s.scope_blocks {
            scope_blocks.push((Origin::Author, sb));
        }
    }

    // E11-M2: memoize per-element selector matches across the whole walk.
    let mut cache = CascadeCache::new(&active);

    // E33-M3: per-shadow-root scoped stylesheets. For each host with a shadow
    // root, parse every `<style>` element's text within that root's subtree
    // (plain-children DFS — nested shadow roots aren't in the child chain, so
    // they form separate scopes automatically). Owned here for the whole fn.
    let shadow_owned: Vec<(NodeId, Vec<Stylesheet>)> = doc
        .shadow_hosts()
        .into_iter()
        .filter_map(|host| {
            let sr = doc.shadow_root(host)?;
            let sheets: Vec<Stylesheet> = gather_shadow_styles(doc, sr)
                .into_iter()
                .map(|css| starfish_css::parse_stylesheet(&css))
                .collect();
            Some((sr, sheets))
        })
        .collect();

    // sr → its active rules (UA + the scoped sheets). Document author sheets are
    // intentionally excluded (encapsulation). Also sr → all scoped `&Rule`s for
    // the `:host`/`::slotted` lookups.
    let mut scoped_active: ScopedActive = HashMap::new();
    let mut shadow_rules: HashMap<NodeId, Vec<&Rule>> = HashMap::new();
    let mut scoped_caches: HashMap<NodeId, CascadeCache> = HashMap::new();
    for (sr, sheets) in &shadow_owned {
        let mut sa: ActiveSheets = vec![(Origin::UserAgent, active_rules(&ua, vp))];
        let mut rules: Vec<&Rule> = Vec::new();
        for s in sheets {
            sa.push((Origin::Author, active_rules(s, vp)));
            rules.extend(s.rules.iter());
        }
        scoped_caches.insert(*sr, CascadeCache::new(&sa));
        scoped_active.insert(*sr, sa);
        shadow_rules.insert(*sr, rules);
    }

    let mut tree = StyledTree::default();
    let mut parent_initial = ComputedStyle::initial();
    // E30-M2: seed the root's custom properties with every @property's
    // initial-value, so an unresolved `var(--registered)` falls back to it
    // (inherited down the tree). A real `--x` declaration overrides it.
    let mut registered: HashMap<String, Vec<starfish_css::Component>> = HashMap::new();
    for s in author_sheets {
        for pr in &s.property_rules {
            registered.insert(pr.name.clone(), pr.initial.clone());
        }
    }
    if !registered.is_empty() {
        parent_initial.custom_props = std::rc::Rc::new(registered);
    }
    // E42-M3: gather every `@counter-style` (author sheets, last wins) into a
    // name → resolved-data map, threaded into each element's cascade context
    // (`EmContext`) so `list-style-type: <name>` resolves against it. Owned here
    // for the whole walk; empty (the common case) keeps pages byte-identical.
    let mut counter_styles: HashMap<String, CounterStyleData> = HashMap::new();
    for s in author_sheets {
        for cr in &s.counter_style_rules {
            counter_styles.insert(
                cr.name.clone(),
                CounterStyleData {
                    system: CounterSystem::from_keyword(&cr.system),
                    symbols: cr.symbols.clone(),
                    prefix: cr.prefix.clone(),
                    suffix: cr.suffix.clone(),
                },
            );
        }
    }
    let root_font_size = parent_initial.font_size;

    // E16-M1: live counter stack, threaded through the pre-order walk.
    let mut counters = counters::CounterState::default();

    // E33-M3: bundle the shadow scopes (empty maps on the non-shadow path).
    let mut scopes = Scopes {
        scoped_active: &scoped_active,
        scoped_caches: &mut scoped_caches,
        shadow_rules: &shadow_rules,
    };

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
            &mut scopes,
            &mut counters,
            &container_blocks,
            &scope_blocks, // E62-M1
            0.0,
            0.0,
            None,
            sizes,
            &counter_styles,
        );
    }
    tree
}

/// Flatten one stylesheet to its viewport-active rules in TRUE source order,
/// each paired with its layer rank (E24-M2; `UNLAYERED` outside `@layer`): a
/// k-way merge of the top-level `rules` with the captured `@media`/`@supports`/
/// `@layer` blocks. Blocks are emitted just BEFORE the top-level rule at their
/// `source_index` (i.e. where they appeared in source), blocks sharing an index
/// ordered by `at_ordinal`. Non-matching media/supports blocks are excluded.
/// With no captured blocks this yields the top-level rules in identical order
/// (byte-identical regression path).
fn active_rules(sheet: &Stylesheet, vp: Viewport) -> Vec<(&Rule, u32)> {
    if sheet.media_blocks.is_empty()
        && sheet.supports_blocks.is_empty()
        && sheet.layer_blocks.is_empty()
    {
        return sheet.rules.iter().map(|r| (r, UNLAYERED)).collect();
    }
    let mb = &sheet.media_blocks;
    let sb = &sheet.supports_blocks;
    let lb = &sheet.layer_blocks;
    let mut out: Vec<(&Rule, u32)> = Vec::new();
    let (mut mi, mut si, mut li) = (0usize, 0usize, 0usize);
    // idx == rules.len() flushes the trailing blocks.
    for idx in 0..=sheet.rules.len() {
        // Emit every block opening at or before this rule's index, smallest
        // at_ordinal first across the three kinds.
        loop {
            let m = mb.get(mi).filter(|b| b.source_index <= idx);
            let s = sb.get(si).filter(|b| b.source_index <= idx);
            let l = lb.get(li).filter(|b| b.source_index <= idx);
            let Some(min) = [
                m.map(|b| b.at_ordinal),
                s.map(|b| b.at_ordinal),
                l.map(|b| b.at_ordinal),
            ]
            .into_iter()
            .flatten()
            .min() else {
                break;
            };
            if m.map(|b| b.at_ordinal) == Some(min) {
                let b = &mb[mi];
                mi += 1;
                if media::media_matches(&b.query, vp) {
                    out.extend(b.rules.iter().map(|r| (r, UNLAYERED)));
                }
            } else if s.map(|b| b.at_ordinal) == Some(min) {
                let b = &sb[si];
                si += 1;
                if supports::supports_matches(&b.condition, vp) {
                    out.extend(b.rules.iter().map(|r| (r, UNLAYERED)));
                }
            } else {
                let b = &lb[li];
                li += 1;
                // Blocks register their name in layer_order, so position()
                // always finds it; UNLAYERED is a defensive fallback.
                let rank = sheet
                    .layer_order
                    .iter()
                    .position(|n| *n == b.name)
                    .map(|p| p as u32)
                    .unwrap_or(UNLAYERED);
                out.extend(b.rules.iter().map(|r| (r, rank)));
            }
        }
        if idx < sheet.rules.len() {
            out.push((&sheet.rules[idx], UNLAYERED));
        }
    }
    out
}

/// E33-M3: gather the CSS text of every `<style>` element within `sr`'s shadow
/// subtree, in document order. Plain-children DFS: nested shadow roots are not
/// in the child chain, so they (correctly) form separate scopes.
fn gather_shadow_styles(doc: &Document, sr: NodeId) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = doc.children(sr);
    stack.reverse(); // process in document order
    while let Some(n) = stack.pop() {
        if doc.tag_name(n) == Some("style") {
            let mut text = String::new();
            for c in doc.children(n) {
                if let starfish_dom::NodeKind::Text(t) = doc.kind(c) {
                    text.push_str(t);
                }
            }
            out.push(text);
        }
        let mut kids = doc.children(n);
        kids.reverse();
        stack.extend(kids);
    }
    out
}

/// E33-M3: shadow-scope bundle threaded alongside the document scope. Empty maps
/// on the non-shadow path → every node uses the document `sheets`/`cache`.
struct Scopes<'a> {
    /// sr → its active rules (UA + scoped sheets).
    scoped_active: &'a ScopedActive<'a>,
    /// sr → per-scope match cache.
    scoped_caches: &'a mut HashMap<NodeId, CascadeCache>,
    /// sr → all of that scope's rules (for `:host` / `::slotted` lookup).
    shadow_rules: &'a HashMap<NodeId, Vec<&'a Rule>>,
}

// --- E64-M3: HTML `dir=auto` first-strong-character directionality ---

/// Whether `node`'s directionality should be auto-resolved from its text
/// content (HTML `dir=auto`). True when the element carries `dir` whose value
/// is the ASCII-case-insensitive keyword `auto`, OR it is a `<bdi>` with no
/// `dir` attribute (bdi's default directionality is auto). An explicit
/// `<bdi dir=ltr>` / `<bdi dir=rtl>` has a `dir` value other than `auto`, so it
/// returns false here and keeps the cascaded value from M1's `[dir=ltr/rtl]`.
fn is_auto_direction(doc: &Document, node: NodeId) -> bool {
    match doc.get_attribute(node, "dir") {
        Some(v) => v.eq_ignore_ascii_case("auto"),
        None => doc.tag_name(node) == Some("bdi"),
    }
}

/// Append `node`'s descendant text (document order) into `out`, skipping the
/// subtrees of elements that establish their own directionality per the HTML
/// `dir=auto` algorithm: `bdi`, `script`, `style`, `textarea`, and any element
/// carrying its own `dir` attribute. `node` itself is always descended into.
fn collect_auto_text(doc: &Document, node: NodeId, out: &mut String) {
    for child in doc.children(node) {
        match doc.kind(child) {
            starfish_dom::NodeKind::Text(t) => out.push_str(t),
            starfish_dom::NodeKind::Element(_) => {
                let tag = doc.tag_name(child).unwrap_or("");
                let excluded = matches!(tag, "bdi" | "script" | "style" | "textarea")
                    || doc.get_attribute(child, "dir").is_some();
                if !excluded {
                    collect_auto_text(doc, child, out);
                }
            }
            _ => {}
        }
    }
}

/// First-strong-character directionality (HTML/UBA rules P2–P3): the direction
/// of the first strong (L vs R/AL) character in `text`, defaulting to LTR when
/// none is found. `unicode_bidi::BidiInfo::new(text, None)` resolves the
/// auto paragraph level by exactly this rule, so its first paragraph's level
/// gives the answer.
fn first_strong_direction(text: &str) -> Direction {
    let info = unicode_bidi::BidiInfo::new(text, None);
    match info.paragraphs.first() {
        Some(p) if p.level.is_rtl() => Direction::Rtl,
        _ => Direction::Ltr,
    }
}

#[allow(clippy::too_many_arguments)]
fn style_node(
    doc: &Document,
    node: NodeId,
    parent_style: &ComputedStyle,
    sheets: &ActiveSheets,
    vp: Viewport,
    root_font_size: &mut f32,
    tree: &mut StyledTree,
    cache: &mut CascadeCache,
    scopes: &mut Scopes,
    counters: &mut counters::CounterState,
    // E25-M1 container-query threading: all blocks, the nearest query
    // container's size/name, and the measured-size map (Some on the 2nd pass).
    container_blocks: &[(Origin, &ContainerBlock)],
    // E62-M1: all `@scope` blocks (UA + author); empty on the byte-identical path.
    scope_blocks: &[(Origin, &starfish_css::ScopeBlock)],
    cq_inline: f32,
    cq_block: f32,
    cq_name: Option<&str>,
    sizes: Option<&HashMap<NodeId, (f32, f32)>>,
    // E42-M3: the document's registered `@counter-style` map.
    counter_styles: &HashMap<String, CounterStyleData>,
) {
    // Only element nodes are styled; descend through their children.
    if doc.tag_name(node).is_none() {
        return;
    }

    let mut style = parent_style.inherit_from();
    // cq* units resolve against the nearest query container (0 on the 1st pass).
    let vp_eff = vp.with_container(cq_inline, cq_block);
    let ctx = EmContext {
        parent_font_size: parent_style.font_size,
        root_font_size: *root_font_size,
        viewport: vp_eff,
        counter_styles, // E42-M3
    };
    // Container rules are only evaluated on the 2nd pass (sizes known).
    let cenv = if sizes.is_some() && !container_blocks.is_empty() {
        ContainerEnv {
            blocks: container_blocks,
            inline: cq_inline,
            block: cq_block,
            name: cq_name,
        }
    } else {
        ContainerEnv::none()
    };
    // E33-M3: pick the scope-appropriate sheets + cache by the node's PHYSICAL
    // position. A node inside a shadow tree matches that scope's sheets only
    // (encapsulation); a light node keeps the document scope (byte-identical
    // when there are no shadow roots). `:host` rules are added on the host, and
    // `::slotted` rules on a distributed light child, on top of its own cascade.
    let scope_sr = doc.enclosing_shadow_root(node);
    let host_rules: &[&Rule] = doc
        .shadow_root(node)
        .and_then(|sr| scopes.shadow_rules.get(&sr))
        .map_or(&[][..], |v| v.as_slice());
    let slotted_rules: &[&Rule] = doc
        .assigned_slot(node)
        .and_then(|slot| doc.enclosing_shadow_root(slot))
        .and_then(|sr2| scopes.shadow_rules.get(&sr2))
        .map_or(&[][..], |v| v.as_slice());
    let (scope_sheets, scope_cache): (&ActiveSheets, &mut CascadeCache) =
        match scope_sr {
            Some(sr) => (
                &scopes.scoped_active[&sr],
                scopes.scoped_caches.get_mut(&sr).expect("scope cache"),
            ),
            None => (sheets, cache),
        };
    cascade(
        doc,
        node,
        scope_sheets,
        ctx,
        &mut style,
        scope_cache,
        cenv,
        host_rules,
        slotted_rules,
        scope_blocks, // E62-M1
    );

    // The first styled element (the root element, e.g. <html>) defines `rem`.
    if doc.tag_name(node) == Some("html") {
        *root_font_size = style.font_size;
    }

    // E64-M3: `dir=auto` (and a bare `<bdi>`) resolve `direction` from the first
    // strong directional character of their text content, overriding the cascade.
    // Applied before the subtree recurses so inherited `direction` flows down.
    // No `dir=auto`/`<bdi>` => untouched (byte-identical path).
    if is_auto_direction(doc, node) {
        let mut text = String::new();
        collect_auto_text(doc, node, &mut text);
        style.direction = first_strong_direction(&text);
    }

    // E16-M1: apply this element's counter operations. `counter-reset` opens a
    // scope (pushed values popped after the subtree); `counter-increment`
    // accumulates and persists for later siblings.
    let pushed = counters.apply_reset(&style.counter_reset);
    counters.apply_increment(&style.counter_increment);

    // E7-M2: ::before / ::after generated-content pseudos. `counter()`/
    // `counters()` in their content read the now-updated counter state.
    // E35-M1: `::marker` joins the pseudo cascade loop.
    // E35-M2: `::placeholder` likewise.
    // E35-M3: `::first-letter` likewise.
    // E53-M3: `::before` is resolved BEFORE the subtree and `::after` AFTER it, so
    // an `open-quote` in `::before` raises the quote depth seen by descendants and
    // a `close-quote` in `::after` lowers it once the subtree is done (correct
    // nesting for `<q><q></q></q>`).
    for side in [
        PseudoElement::Before,
        PseudoElement::Marker,
        PseudoElement::Placeholder,
        PseudoElement::FirstLetter,
    ] {
        // E33-M3: pseudo-elements cascade in the node's own scope sheets.
        if let Some(entry) =
            cascade_pseudo(doc, node, side.clone(), &style, scope_sheets, ctx, counters)
        {
            match side {
                PseudoElement::Before => tree.before.insert(node, entry),
                PseudoElement::Marker => tree.marker.insert(node, entry),
                PseudoElement::Placeholder => tree.placeholder.insert(node, entry),
                PseudoElement::FirstLetter => tree.first_letter.insert(node, entry),
                PseudoElement::After | PseudoElement::Slotted(_) => {
                    unreachable!("::after resolved after the subtree; only B/M/P/F iterated")
                }
            };
        }
    }

    // E25-M1: if this element is a query container, its measured content-box
    // size becomes the scope for descendants (block extent only for `size`
    // containers; `inline-size` containers leave the block axis unqueryable).
    // Otherwise descendants inherit the current nearest container.
    let (child_inline, child_block, child_name) = if style.container_type != ContainerType::Normal {
        match sizes.and_then(|m| m.get(&node)) {
            Some(&(w, h)) => {
                let block = if style.container_type == ContainerType::Size {
                    h
                } else {
                    0.0
                };
                (w, block, style.container_name.as_deref())
            }
            None => (cq_inline, cq_block, cq_name),
        }
    } else {
        (cq_inline, cq_block, cq_name)
    };

    // E33-M2: composed-tree walk. A shadow host recurses into its shadow tree
    // and a `<slot>` expands to its assigned light children (or fallback content
    // when empty); a non-shadow, non-slot element has `composed_children ==
    // children`, so this is byte-identical on the default path. Slotted/shadow
    // children inherit from this flattened parent (`&style`) — scoped matching is
    // M3.
    for child in doc.composed_children(node) {
        style_node(
            doc,
            child,
            &style,
            sheets,
            vp,
            root_font_size,
            tree,
            cache,
            scopes,
            counters,
            container_blocks,
            scope_blocks, // E62-M1
            child_inline,
            child_block,
            child_name,
            sizes,
            counter_styles,
        );
    }

    // E53-M3: `::after` is resolved AFTER the subtree so a `close-quote` lowers the
    // quote depth only once all descendants (which may themselves open/close
    // quotes) have been processed. Other pseudos are resolved before the subtree.
    if let Some(entry) = cascade_pseudo(
        doc,
        node,
        PseudoElement::After,
        &style,
        scope_sheets,
        ctx,
        counters,
    ) {
        tree.after.insert(node, entry);
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
        // E59-M1: sample every animation on the element in order, so a later one
        // wins on a property both touch. A single animation reduces exactly to the
        // E17 one-animation path.
        let anims = match tree.styles.get(&id) {
            Some(s) if !s.animation.is_empty() => s.animation.clone(),
            _ => continue,
        };
        let style = tree.styles.get_mut(&id).unwrap();
        sample_animations_into(style, &anims, &kf_by_name, at_seconds, root_font_size, vp);
    }

    // E59-M3: pseudo-element animations. The `::before`/`::after` cascade already
    // computes `animation`/`transition` onto the pseudo's ComputedStyle (via the
    // same `apply_declaration` path as elements), so sampling is the identical
    // helper applied to the pseudo style. A pseudo with no animation is untouched.
    for map in [&mut tree.before, &mut tree.after] {
        for entry in map.values_mut() {
            let (style, _content) = &mut **entry;
            if style.animation.is_empty() {
                continue;
            }
            let anims = style.animation.clone();
            sample_animations_into(style, &anims, &kf_by_name, at_seconds, root_font_size, vp);
        }
    }
}

/// E59-M3: sample `anims` (already cloned off the style) onto one `ComputedStyle`
/// at clock `at_seconds`. Factored out of `apply_animations` so both element and
/// `::before`/`::after` pseudo styles run the identical sampling logic. The
/// style's own `font_size` is the `em` basis (matching the element path, where
/// `parent_font_size` was the element's font-size).
fn sample_animations_into(
    style: &mut ComputedStyle,
    anims: &[Animation],
    kf_by_name: &HashMap<&str, &KeyframesRule>,
    at_seconds: f32,
    root_font_size: f32,
    vp: Viewport,
) {
    for anim in anims {
        let Some(kf) = kf_by_name.get(anim.name.as_str()) else {
            continue;
        };

        // Eased progress at this clock, honouring delay / iteration-count /
        // direction / fill-mode. `None` => no override applies (the cascaded
        // base value wins — fill-mode None outside the active span).
        let p = match resolve_progress(anim, at_seconds) {
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

        // E42-M3: list-style-type is not animatable, so no @counter-style map
        // is needed for the animation sampling pass.
        let no_counter_styles = HashMap::new();
        let ctx = EmContext {
            parent_font_size: style.font_size,
            root_font_size,
            viewport: vp,
            counter_styles: &no_counter_styles,
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

            apply_interpolated(style, prop, lo_decl, hi_decl, local_t, ctx);
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
    let easing = &anim.timing; // E59-M2: `Easing` is no longer `Copy`.
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
    ctx: EmContext<'_>,
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
        "width" => style.width = lerp_length(&scratch_lo.width, &scratch_hi.width, t),
        "height" => style.height = lerp_length(&scratch_lo.height, &scratch_hi.height, t),
        "margin-top" => {
            style.margin_top = lerp_length(&scratch_lo.margin_top, &scratch_hi.margin_top, t)
        }
        "margin-right" => {
            style.margin_right = lerp_length(&scratch_lo.margin_right, &scratch_hi.margin_right, t)
        }
        "margin-bottom" => {
            style.margin_bottom =
                lerp_length(&scratch_lo.margin_bottom, &scratch_hi.margin_bottom, t)
        }
        "margin-left" => {
            style.margin_left = lerp_length(&scratch_lo.margin_left, &scratch_hi.margin_left, t)
        }
        "padding-top" => {
            style.padding_top = lerp_length(&scratch_lo.padding_top, &scratch_hi.padding_top, t)
        }
        "padding-right" => {
            style.padding_right =
                lerp_length(&scratch_lo.padding_right, &scratch_hi.padding_right, t)
        }
        "padding-bottom" => {
            style.padding_bottom =
                lerp_length(&scratch_lo.padding_bottom, &scratch_hi.padding_bottom, t)
        }
        "padding-left" => {
            style.padding_left = lerp_length(&scratch_lo.padding_left, &scratch_hi.padding_left, t)
        }
        "top" => style.top = lerp_length(&scratch_lo.top, &scratch_hi.top, t),
        "right" => style.right = lerp_length(&scratch_lo.right, &scratch_hi.right, t),
        "bottom" => style.bottom = lerp_length(&scratch_lo.bottom, &scratch_hi.bottom, t),
        "left" => style.left = lerp_length(&scratch_lo.left, &scratch_hi.left, t),
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
        let to = tree.styles.get_mut(&id).unwrap();
        sample_transitions_into(to, &transitions, &from, at_seconds);
    }

    // E59-M3: pseudo-element transitions. A `::before`/`::after` transition is
    // sampled from→to like an element one, when the from-snapshot has the matching
    // pseudo entry (same element id, same side). Single-property MVP per the
    // roadmap non-goal; reuses the identical per-style helper.
    for side in [PseudoElement::Before, PseudoElement::After] {
        let from_map = match side {
            PseudoElement::Before => &from_tree.before,
            _ => &from_tree.after,
        };
        let to_map = match side {
            PseudoElement::Before => &mut tree.before,
            _ => &mut tree.after,
        };
        for (id, entry) in to_map.iter_mut() {
            let (to_style, _content) = &mut **entry;
            if to_style.transitions.is_empty() {
                continue;
            }
            let Some(from_entry) = from_map.get(id) else {
                continue;
            };
            let transitions = to_style.transitions.clone();
            let from = from_entry.0.clone();
            sample_transitions_into(to_style, &transitions, &from, at_seconds);
        }
    }
}

/// E59-M3: sample `transitions` (already cloned off the `to` style) onto one
/// `ComputedStyle`, interpolating each watched property from `from` toward the
/// current `to` value at clock `at_seconds`. Factored out of `apply_transitions`
/// so element and `::before`/`::after` pseudo styles share the logic.
fn sample_transitions_into(
    to: &mut ComputedStyle,
    transitions: &[Transition],
    from: &ComputedStyle,
    at_seconds: f32,
) {
    for prop in TRANSITIONABLE {
        // Last transition entry that watches this property wins.
        let entry = transitions.iter().rev().find(|tr| match &tr.property {
            TransitionProp::All => true,
            TransitionProp::Name(n) => n == prop,
        });
        let Some(entry) = entry else { continue };
        let p = resolve_transition_progress(entry, at_seconds);
        lerp_field(to, prop, from, p);
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
        "width" => lerp_f!(width, |a, b| lerp_length(a, b, t)),
        "height" => lerp_f!(height, |a, b| lerp_length(a, b, t)),
        "margin-top" => lerp_f!(margin_top, |a, b| lerp_length(a, b, t)),
        "margin-right" => lerp_f!(margin_right, |a, b| lerp_length(a, b, t)),
        "margin-bottom" => lerp_f!(margin_bottom, |a, b| lerp_length(a, b, t)),
        "margin-left" => lerp_f!(margin_left, |a, b| lerp_length(a, b, t)),
        "padding-top" => lerp_f!(padding_top, |a, b| lerp_length(a, b, t)),
        "padding-right" => lerp_f!(padding_right, |a, b| lerp_length(a, b, t)),
        "padding-bottom" => lerp_f!(padding_bottom, |a, b| lerp_length(a, b, t)),
        "padding-left" => lerp_f!(padding_left, |a, b| lerp_length(a, b, t)),
        "top" => lerp_f!(top, |a, b| lerp_length(a, b, t)),
        "right" => lerp_f!(right, |a, b| lerp_length(a, b, t)),
        "bottom" => lerp_f!(bottom, |a, b| lerp_length(a, b, t)),
        "left" => lerp_f!(left, |a, b| lerp_length(a, b, t)),
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

    // --- E24-M3: color-mix / env / expanded attr ---

    #[test]
    fn color_mix_in_property_value() {
        // color-mix(in srgb, red, blue) → purple (128, 0, 128).
        let (doc, t) = style("<p>x</p>", "p { color: color-mix(in srgb, red, blue) }");
        assert_eq!(
            t.computed(find(&doc, "p")).color,
            Rgba {
                r: 128,
                g: 0,
                b: 128,
                a: 255
            }
        );
    }

    // E44-M3
    #[test]
    fn color_function_in_property_value() {
        // color(srgb 1 0 0) → red through the property parser.
        let (doc, t) = style("<p>x</p>", "p { color: color(srgb 1 0 0) }");
        assert_eq!(
            t.computed(find(&doc, "p")).color,
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );
        // hwb(0 0% 0%) → red as well.
        let (doc2, t2) = style("<p>x</p>", "p { color: hwb(0 0% 0%) }");
        assert_eq!(
            t2.computed(find(&doc2, "p")).color,
            Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            }
        );
    }

    #[test]
    fn env_resolves_to_fallback() {
        // No device chrome → env() uses its fallback length.
        let (doc, t) = style("<p>x</p>", "p { width: env(safe-area-inset-top, 10px) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Px(10.0));
        // No fallback → 0.
        let (doc2, t2) = style("<p>x</p>", "p { width: env(safe-area-inset-left) }");
        assert_eq!(t2.computed(find(&doc2, "p")).width, Length::Px(0.0));
    }

    #[test]
    fn env_in_shorthand_resolves_like_var() {
        // E42-M2: env() resolves on the var() slow path, so it works inside a
        // multi-value shorthand (top/bottom from fallback, left/right from 0).
        let (doc, t) = style(
            "<p>x</p>",
            "p { padding: env(safe-area-inset-top, 20px) env(x, 40px) }",
        );
        let s = t.computed(find(&doc, "p"));
        assert_eq!(s.padding_top, Length::Px(20.0));
        assert_eq!(s.padding_right, Length::Px(40.0));
        assert_eq!(s.padding_bottom, Length::Px(20.0));
    }

    #[test]
    fn attr_typed_length_in_width() {
        // attr(data-w px) sets the width from the attribute.
        let (doc, t) = style("<p data-w=\"120\">x</p>", "p { width: attr(data-w px) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Px(120.0));
        // Missing attribute → fallback.
        let (doc2, t2) = style("<p>x</p>", "p { width: attr(data-w px, 40px) }");
        assert_eq!(t2.computed(find(&doc2, "p")).width, Length::Px(40.0));
        // Missing attribute, no fallback → type default (0).
        let (doc3, t3) = style("<p>x</p>", "p { width: attr(data-w px) }");
        assert_eq!(t3.computed(find(&doc3, "p")).width, Length::Px(0.0));
    }

    // --- E27-M1: @media range syntax ---

    #[test]
    fn media_range_min_gte() {
        let css = "@media (width >= 400px) { p { color: red } }";
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(400.0));
        assert_eq!(t.computed(find(&d, "p")).color, red());
        let (d2, t2) = style_vp("<p>x</p>", css, Viewport::from_width(399.0));
        assert_eq!(t2.computed(find(&d2, "p")).color, black());
    }

    #[test]
    fn media_range_interval_half_open() {
        // [400, 800): 400 and 600 match, 800 and 399 don't.
        let css = "@media (400px <= width < 800px) { p { color: red } }";
        let matches = |w: f32| {
            let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(w));
            t.computed(find(&d, "p")).color == red()
        };
        assert!(matches(400.0));
        assert!(matches(600.0));
        assert!(!matches(800.0));
        assert!(!matches(399.0));
    }

    #[test]
    fn media_range_value_first_form() {
        // `(600px > width)` ⇒ width < 600.
        let css = "@media (600px > width) { p { color: red } }";
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(500.0));
        assert_eq!(t.computed(find(&d, "p")).color, red());
        let (d2, t2) = style_vp("<p>x</p>", css, Viewport::from_width(700.0));
        assert_eq!(t2.computed(find(&d2, "p")).color, black());
    }

    // --- E29-M1: type-indexed + last structural pseudos ---

    #[test]
    fn of_type_and_last_pseudos() {
        let html = "<div><p id=p1>a</p><span id=s1>b</span><p id=p2>c</p></div>";
        let (d, t) = style(html, "p:last-of-type { color: red }");
        assert_eq!(t.computed(find_id(&d, "p2")).color, red());
        assert_eq!(t.computed(find_id(&d, "p1")).color, black());
        let (d2, t2) = style(html, "span:only-of-type { color: red }");
        assert_eq!(t2.computed(find_id(&d2, "s1")).color, red());
        // `p:nth-last-child(1)` → the last child, when it's a <p> (p2).
        let (d3, t3) = style(html, "p:nth-last-child(1) { color: red }");
        assert_eq!(t3.computed(find_id(&d3, "p2")).color, red());
        assert_eq!(t3.computed(find_id(&d3, "p1")).color, black());
    }

    // --- E29-M2: `of S` argument + case-sensitivity flag ---

    #[test]
    fn nth_child_of_selector() {
        let html =
            "<div><p>x</p><span id=i1 class=item>a</span><p id=i2 class=item>b</p></div>";
        // :nth-child(1 of .item) → the FIRST .item (i1), skipping the non-.item <p>.
        let (d, t) = style(html, ":nth-child(1 of .item) { color: red }");
        assert_eq!(t.computed(find_id(&d, "i1")).color, red());
        assert_eq!(t.computed(find_id(&d, "i2")).color, black());
        // 2 of .item → i2.
        let (d2, t2) = style(html, ":nth-child(2 of .item) { color: red }");
        assert_eq!(t2.computed(find_id(&d2, "i2")).color, red());
    }

    #[test]
    fn attr_case_sensitive_flag() {
        let html = "<p id=a data-x=A>x</p><p id=b data-x=a>y</p>";
        let (d, t) = style(html, "[data-x=A s] { color: red }");
        assert_eq!(t.computed(find_id(&d, "a")).color, red());
        assert_eq!(t.computed(find_id(&d, "b")).color, black());
    }

    // --- E29-M3: link + UI pseudos ---

    #[test]
    fn link_and_placeholder_pseudos() {
        // :link → <a href> only.
        let (d, t) = style("<a id=l href=x>x</a><a id=n>y</a>", "a:link { color: red }");
        assert_eq!(t.computed(find_id(&d, "l")).color, red());
        assert_eq!(t.computed(find_id(&d, "n")).color, black());
        // :placeholder-shown → empty placeholdered input only.
        let (d2, t2) = style(
            "<input id=e placeholder=p><input id=f placeholder=p value=v>",
            ":placeholder-shown { color: red }",
        );
        assert_eq!(t2.computed(find_id(&d2, "e")).color, red());
        assert_eq!(t2.computed(find_id(&d2, "f")).color, black());
    }

    #[test]
    fn lang_pseudo_matches_prefix() {
        let (d, t) = style(
            "<div lang=en-US><p id=p>x</p></div><p id=q>y</p>",
            ":lang(en) { color: red }",
        );
        assert_eq!(t.computed(find_id(&d, "p")).color, red());
        assert_eq!(t.computed(find_id(&d, "q")).color, black());
    }

    // --- E32-M1: clip-path ---

    #[test]
    fn clip_path_shapes_parse() {
        use computed::{ClipShape, LengthPct};
        let (d, t) = style("<p>x</p>", "p { clip-path: circle(40% at 50% 50%) }");
        assert!(matches!(
            t.computed(find(&d, "p")).clip_path,
            Some(ClipShape::Circle { .. })
        ));
        let (d2, t2) = style("<p>x</p>", "p { clip-path: polygon(0 0, 100% 0, 50% 100%) }");
        match &t2.computed(find(&d2, "p")).clip_path {
            Some(ClipShape::Polygon(pts)) => assert_eq!(pts.len(), 3),
            other => panic!("got {other:?}"),
        }
        let (d3, t3) = style("<p>x</p>", "p { clip-path: inset(10px) }");
        assert!(matches!(
            t3.computed(find(&d3, "p")).clip_path,
            Some(ClipShape::Inset { top: LengthPct::Px(10.0), .. })
        ));
        let (d4, t4) = style("<p>x</p>", "p { clip-path: none }");
        assert!(t4.computed(find(&d4, "p")).clip_path.is_none());
    }

    // --- E65-M1: shape-outside ---

    #[test]
    fn shape_outside_parses() {
        use computed::ClipShape;
        let (d, t) = style("<p>x</p>", "p { shape-outside: circle(50%) }");
        match &t.computed(find(&d, "p")).shape_outside {
            Some(b) => assert!(matches!(**b, ClipShape::Circle { .. })),
            None => panic!("expected Some(Box<Circle>)"),
        }
        let (d2, t2) = style("<p>x</p>", "p { shape-outside: none }");
        assert!(t2.computed(find(&d2, "p")).shape_outside.is_none());
    }

    #[test]
    fn shape_margin_parses() {
        let (d, t) = style("<p>x</p>", "p { shape-margin: 10px }");
        assert_eq!(t.computed(find(&d, "p")).shape_margin, 10.0);
        let (d2, t2) = style("<p>x</p>", "p { color: red }");
        assert_eq!(t2.computed(find(&d2, "p")).shape_margin, 0.0);
    }

    // --- E30-M2: @property initial-value fallback ---

    #[test]
    fn property_initial_value_fallback() {
        // var(--c) with no declared --c falls back to the @property initial.
        let css = "@property --c { syntax: \"<color>\"; inherits: false; initial-value: red } \
                   p { color: var(--c) }";
        let (d, t) = style("<p>x</p>", css);
        assert_eq!(t.computed(find(&d, "p")).color, red());
        // A real --c declaration overrides the initial.
        let css2 = "@property --c { syntax: \"<color>\"; inherits: false; initial-value: red } \
                    p { --c: blue; color: var(--c) }";
        let (d2, t2) = style("<p>x</p>", css2);
        assert_eq!(
            t2.computed(find(&d2, "p")).color,
            Rgba { r: 0, g: 0, b: 255, a: 255 }
        );
    }

    // --- E28-M3: nested @media ---

    #[test]
    fn nested_media_applies_conditionally() {
        let css = ".card { color: green; @media (width >= 400px) { color: red } }";
        // Wide viewport → nested @media wins.
        let (d, t) = style_vp("<div class=card>x</div>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find_class(&d, "card")).color, red());
        // Narrow viewport → only the base color.
        let (d2, t2) = style_vp("<div class=card>x</div>", css, Viewport::from_width(300.0));
        assert_eq!(t2.computed(find_class(&d2, "card")).color, green());
    }

    // --- E27-M3: dimensional media features ---

    #[test]
    fn min_aspect_ratio_matches_landscape() {
        let css = "@media (min-aspect-ratio: 1/1) { p { color: red } }";
        // 800×600 is landscape (4:3 > 1) → match.
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find(&d, "p")).color, red());
        // A portrait viewport → no match.
        let mut vp = Viewport::from_width(400.0);
        vp.height = 800.0;
        let (d2, t2) = style_vp("<p>x</p>", css, vp);
        assert_eq!(t2.computed(find(&d2, "p")).color, black());
    }

    #[test]
    fn min_resolution_matches_dpr() {
        let css = "@media (min-resolution: 2dppx) { p { color: red } }";
        // Default dpr 1 → no match.
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find(&d, "p")).color, black());
        let mut vp = Viewport::from_width(800.0);
        vp.prefs.dpr = 2.0;
        let (d2, t2) = style_vp("<p>x</p>", css, vp);
        assert_eq!(t2.computed(find(&d2, "p")).color, red());
    }

    // --- E27-M2: user-preference media features ---

    #[test]
    fn prefers_color_scheme_matches_request() {
        let css = "@media (prefers-color-scheme: dark) { p { color: red } }";
        // Default is light → no match.
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find(&d, "p")).color, black());
        // Request dark → match.
        let mut vp = Viewport::from_width(800.0);
        vp.prefs.color_scheme = ColorScheme::Dark;
        let (d2, t2) = style_vp("<p>x</p>", css, vp);
        assert_eq!(t2.computed(find(&d2, "p")).color, red());
    }

    #[test]
    fn pointer_coarse_does_not_match_default_fine() {
        let css = "@media (pointer: coarse) { p { color: red } }";
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find(&d, "p")).color, black());
        let mut vp = Viewport::from_width(800.0);
        vp.prefs.pointer = PointerKind::Coarse;
        let (d2, t2) = style_vp("<p>x</p>", css, vp);
        assert_eq!(t2.computed(find(&d2, "p")).color, red());
    }

    // --- E25-M1: container queries + cq units ---

    fn container_sizes(pairs: &[(NodeId, (f32, f32))]) -> HashMap<NodeId, (f32, f32)> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn container_query_min_width_gates_on_size() {
        let html = "<div class=card><p class=title>x</p></div>";
        let css = ".card{container-type:inline-size} \
                   @container (min-width:400px){ .title{color:red} }";
        let doc = parse(html);
        let card = find_class(&doc, "card");
        // Wide container → the rule applies.
        let wide = container_sizes(&[(card, (500.0, 100.0))]);
        let t = style_tree_containers(
            &doc,
            &[parse_stylesheet(css)],
            Viewport::from_width(800.0),
            &wide,
        );
        assert_eq!(t.computed(find(&doc, "p")).color, red());
        // Narrow container → it does not.
        let narrow = container_sizes(&[(card, (300.0, 100.0))]);
        let t2 = style_tree_containers(
            &doc,
            &[parse_stylesheet(css)],
            Viewport::from_width(800.0),
            &narrow,
        );
        assert_eq!(t2.computed(find(&doc, "p")).color, black());
    }

    #[test]
    fn cqw_unit_resolves_against_container() {
        let html = "<div class=card><p class=title>x</p></div>";
        let css = ".card{container-type:inline-size} .title{width:50cqw}";
        let doc = parse(html);
        let card = find_class(&doc, "card");
        let sizes = container_sizes(&[(card, (400.0, 100.0))]);
        let t = style_tree_containers(
            &doc,
            &[parse_stylesheet(css)],
            Viewport::from_width(800.0),
            &sizes,
        );
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Px(200.0));
    }

    #[test]
    fn named_container_only_matches_its_name() {
        let html = "<div class=card><p class=title>x</p></div>";
        // Query targets `sidebar`, but the container is unnamed → no match.
        let css = ".card{container-type:inline-size} \
                   @container sidebar (min-width:100px){ .title{color:red} }";
        let doc = parse(html);
        let card = find_class(&doc, "card");
        let sizes = container_sizes(&[(card, (500.0, 100.0))]);
        let t = style_tree_containers(
            &doc,
            &[parse_stylesheet(css)],
            Viewport::from_width(800.0),
            &sizes,
        );
        assert_eq!(t.computed(find(&doc, "p")).color, black());
    }

    #[test]
    fn no_container_blocks_is_byte_identical() {
        // A page without @container styles the same whether or not sizes pass in.
        let (doc, t) = style("<p>x</p>", "p { width: 10px }");
        let empty = container_sizes(&[]);
        let t2 = style_tree_containers(
            &doc,
            &[parse_stylesheet("p { width: 10px }")],
            Viewport::from_width(800.0),
            &empty,
        );
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Px(10.0));
        assert_eq!(t2.computed(find(&doc, "p")).width, Length::Px(10.0));
    }

    // --- E62-M1: @scope (<root>) { rules } ---

    #[test]
    fn scope_styles_descendants_of_root_only() {
        // A `@scope (.card) { p { color:red } }` colors the `<p>` inside `.card`
        // (a descendant of the scope root) but not the `<p>` outside it.
        let html = "<div class=card><p id=in>in</p></div><p id=out>out</p>";
        let css = "@scope (.card) { p { color: red } }";
        let (doc, t) = style(html, css);
        assert_eq!(t.computed(find_id(&doc, "in")).color, red());
        assert_eq!(t.computed(find_id(&doc, "out")).color, black());
    }

    #[test]
    fn scope_root_itself_is_in_scope() {
        // The scope root is descendant-OR-SELF: a rule matching the root element
        // itself applies. `@scope (.card) { .card { … } }` styles `.card`.
        let html = "<div class=card id=c>x</div>";
        let css = "@scope (.card) { .card { color: red } }";
        let (doc, t) = style(html, css);
        assert_eq!(t.computed(find_id(&doc, "c")).color, red());
    }

    // --- E62-M2: @scope (<root>) to (<limit>) { rules } ---

    #[test]
    fn scope_to_limit_bounds_the_subtree() {
        // `@scope (.card) to (.content)` styles `<p>`s between `.card` and the
        // `.content` boundary, but not inside (at/below) `.content`. The
        // `.content` element itself is out of scope (limit is exclusive).
        let html = "<div class=card><p id=a>A</p>\
                    <div class=content id=lim><p id=b>B</p></div></div>";
        let css = "@scope (.card) to (.content) { p { color: red } }";
        let (doc, t) = style(html, css);
        // <p id=a>: descendant of root, no limit crossed → in scope.
        assert_eq!(t.computed(find_id(&doc, "a")).color, red());
        // <p id=b>: inside `.content` (past the limit) → out of scope.
        assert_eq!(t.computed(find_id(&doc, "b")).color, black());
    }

    #[test]
    fn scope_limit_element_itself_is_out() {
        // The limit element matches the inner rule but is itself excluded
        // (limit is exclusive). Uses non-inherited `background-color` so the
        // limit element's exclusion is observable independent of inheritance.
        let html = "<div class=card id=c><div class=content id=lim>x</div></div>";
        let css = "@scope (.card) to (.content) { div { background-color: red } }";
        let (doc, t) = style(html, css);
        // The root `.card` is in scope → gets the background.
        assert_eq!(t.computed(find_id(&doc, "c")).background_color, red());
        // The limit `.content` is out of scope → no background (transparent).
        let transparent = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
        assert_eq!(
            t.computed(find_id(&doc, "lim")).background_color,
            transparent
        );
    }

    #[test]
    fn no_scope_block_is_byte_identical() {
        // A page without @scope styles exactly as before (empty scope_blocks →
        // the cascade's new loop is skipped).
        let (doc, t) = style("<p>x</p>", "p { color: red }");
        assert_eq!(t.computed(find(&doc, "p")).color, red());
    }

    // --- E62-M3: :scope / & in scoped rules + bare @scope ---

    #[test]
    fn scope_pseudo_targets_the_scope_root() {
        // `@scope (.card) { :scope { … } }` styles the `.card` scope root itself,
        // not its descendants. Non-inherited `background-color` makes the target
        // unambiguous (descendants must stay transparent).
        let html = "<div class=card id=c><p id=p>x</p></div>";
        let css = "@scope (.card) { :scope { background-color: red } }";
        let (doc, t) = style(html, css);
        assert_eq!(t.computed(find_id(&doc, "c")).background_color, red());
        let transparent = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
        assert_eq!(t.computed(find_id(&doc, "p")).background_color, transparent);
    }

    #[test]
    fn scope_pseudo_descendant_styles_scoped_descendants() {
        // `:scope p` styles `<p>` descendants of the scope root (the `:scope`
        // part is satisfied by scope membership). The `<p>` outside `.card` is
        // out of scope and unaffected.
        let html = "<div class=card><p id=in>in</p></div><p id=out>out</p>";
        let css = "@scope (.card) { :scope p { color: red } }";
        let (doc, t) = style(html, css);
        assert_eq!(t.computed(find_id(&doc, "in")).color, red());
        assert_eq!(t.computed(find_id(&doc, "out")).color, black());
    }

    #[test]
    fn nesting_amp_descendant_behaves_like_scope() {
        // `& p` inside a scope block behaves like `:scope p` (the `&` refers to
        // the scope root).
        let html = "<div class=card><p id=in>in</p></div><p id=out>out</p>";
        let css = "@scope (.card) { & p { color: red } }";
        let (doc, t) = style(html, css);
        assert_eq!(t.computed(find_id(&doc, "in")).color, red());
        assert_eq!(t.computed(find_id(&doc, "out")).color, black());
    }

    #[test]
    fn nesting_amp_alone_targets_the_scope_root() {
        // A bare `&` targets the scope root itself (like `:scope`).
        let html = "<div class=card id=c><p id=p>x</p></div>";
        let css = "@scope (.card) { & { background-color: red } }";
        let (doc, t) = style(html, css);
        assert_eq!(t.computed(find_id(&doc, "c")).background_color, red());
    }

    #[test]
    fn bare_scope_scopes_to_document_root() {
        // A prelude-less `@scope { p { … } }` scopes to `:root` (MVP), so every
        // `<p>` in the document is styled.
        let html = "<div><p id=a>A</p></div><p id=b>B</p>";
        let css = "@scope { p { color: red } }";
        let (doc, t) = style(html, css);
        assert_eq!(t.computed(find_id(&doc, "a")).color, red());
        assert_eq!(t.computed(find_id(&doc, "b")).color, red());
    }

    // --- E25-M2: logical properties ---

    #[test]
    fn margin_inline_maps_horizontal_ltr() {
        let (doc, t) = style("<p>x</p>", "p { margin-inline: 10px 20px }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.margin_left, Length::Px(10.0));
        assert_eq!(p.margin_right, Length::Px(20.0));
        // margin-inline doesn't touch the block axis (UA p keeps top = 16px).
        assert_eq!(p.margin_top, Length::Px(16.0));
    }

    #[test]
    fn margin_inline_maps_to_block_in_vertical() {
        // vertical-rl: inline axis runs vertically → start=top, end=bottom.
        let (doc, t) = style(
            "<p>x</p>",
            "p { writing-mode: vertical-rl; margin-inline: 10px 20px }",
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.margin_top, Length::Px(10.0));
        assert_eq!(p.margin_bottom, Length::Px(20.0));
    }

    #[test]
    fn inline_start_flips_under_rtl() {
        let (doc, t) = style(
            "<p>x</p>",
            "p { direction: rtl; padding-inline-start: 5px }",
        );
        // RTL inline-start is the right side.
        assert_eq!(t.computed(find(&doc, "p")).padding_right, Length::Px(5.0));
    }

    #[test]
    fn inline_size_maps_to_width_or_height() {
        let (doc, t) = style("<p>x</p>", "p { inline-size: 200px }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Px(200.0));
        let (doc2, t2) = style(
            "<p>x</p>",
            "p { writing-mode: vertical-rl; inline-size: 200px }",
        );
        assert_eq!(t2.computed(find(&doc2, "p")).height, Length::Px(200.0));
    }

    #[test]
    fn text_align_start_end_by_direction() {
        let (doc, t) = style("<p>x</p>", "p { text-align: start }");
        assert_eq!(t.computed(find(&doc, "p")).text_align, TextAlign::Left);
        let (doc2, t2) = style("<p>x</p>", "p { direction: rtl; text-align: start }");
        assert_eq!(t2.computed(find(&doc2, "p")).text_align, TextAlign::Right);
    }

    #[test]
    fn inset_block_maps_top_bottom() {
        let (doc, t) = style("<p>x</p>", "p { inset-block: 3px 7px }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.top, Length::Px(3.0));
        assert_eq!(p.bottom, Length::Px(7.0));
    }

    // --- E25-M3: place-* shorthands ---

    #[test]
    fn place_items_sets_both_axes() {
        use computed::AlignItems;
        let (doc, t) = style("<p>x</p>", "p { place-items: center }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.align_items, AlignItems::Center);
        assert_eq!(p.justify_items, AlignItems::Center);
    }

    #[test]
    fn place_content_two_values() {
        use computed::JustifyContent;
        let (doc, t) = style("<p>x</p>", "p { place-content: space-between center }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.align_content, JustifyContent::SpaceBetween);
        assert_eq!(p.justify_content, JustifyContent::Center);
    }

    #[test]
    fn place_self_single_value_both() {
        use computed::AlignSelf;
        let (doc, t) = style("<p>x</p>", "p { place-self: end }");
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.align_self, AlignSelf::FlexEnd);
        assert_eq!(p.justify_self, AlignSelf::FlexEnd);
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
    fn ua_dialog_and_hidden() {
        // E36-M2: dialog without `open` and any `[hidden]` element → display:none;
        // an `open` dialog is block.
        let doc = parse(
            "<body><dialog open>o</dialog><dialog>c</dialog>\
             <p hidden>x</p><p>y</p></body>",
        );
        let t = style_tree(&doc, &[]);
        // collect all <dialog> / <p> in document order
        let mut dialogs = Vec::new();
        let mut ps = Vec::new();
        let mut stack = vec![doc.root()];
        let mut order = Vec::new();
        while let Some(n) = stack.pop() {
            order.push(n);
            let mut kids: Vec<_> = doc.children(n);
            kids.reverse();
            stack.extend(kids);
        }
        for n in order {
            match doc.tag_name(n) {
                Some("dialog") => dialogs.push(n),
                Some("p") => ps.push(n),
                _ => {}
            }
        }
        assert_eq!(t.computed(dialogs[0]).display, Display::Block); // open
        assert_eq!(t.computed(dialogs[1]).display, Display::None); // closed
        assert_eq!(t.computed(ps[0]).display, Display::None); // [hidden]
        assert_eq!(t.computed(ps[1]).display, Display::Block); // visible
    }

    #[test]
    fn ua_popover_open_flag() {
        // E36-M3: a [popover] element is display:none until its Document open
        // flag is set, then display:block (via [popover]:popover-open).
        let mut doc = parse("<body><div popover>p</div></body>");
        let div = find(&doc, "div");
        let t = style_tree(&doc, &[]);
        assert_eq!(t.computed(div).display, Display::None); // closed → hidden

        doc.set_popover_open(div, true);
        let t = style_tree(&doc, &[]);
        assert_eq!(t.computed(div).display, Display::Block); // open → block
    }

    #[test]
    fn ua_fieldset_legend_datalist() {
        // E39-M2: fieldset/legend are block; datalist is display:none.
        let doc = parse(
            "<body><fieldset><legend>L</legend>x</fieldset>\
             <datalist><option>o</option></datalist></body>",
        );
        let t = style_tree(&doc, &[]);
        assert_eq!(t.computed(find(&doc, "fieldset")).display, Display::Block);
        assert_eq!(t.computed(find(&doc, "legend")).display, Display::Block);
        assert_eq!(t.computed(find(&doc, "datalist")).display, Display::None);
    }

    #[test]
    fn ua_ruby_rt_rp() {
        // E56-M1: ruby → inline-block centered; rt → block, ~50% font-size,
        // centered; rp → display:none.
        let doc = parse("<body><ruby>X<rt>ann</rt><rp>(</rp></ruby></body>");
        let t = style_tree(&doc, &[]);
        let ruby = t.computed(find(&doc, "ruby"));
        assert_eq!(ruby.display, Display::InlineBlock);
        assert_eq!(ruby.text_align, TextAlign::Center);
        let rt = t.computed(find(&doc, "rt"));
        assert_eq!(rt.display, Display::Block);
        assert_eq!(rt.text_align, TextAlign::Center);
        // 50% of the inherited 16px base.
        assert_eq!(rt.font_size, 8.0);
        assert_eq!(t.computed(find(&doc, "rp")).display, Display::None);
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

    // E63-M1: <template> is inert — UA `template { display: none }` makes it
    // (and its parsed content) compute display:none so it is never laid out.
    #[test]
    fn ua_template_none() {
        let doc = parse("<body><template><p>x</p></template></body>");
        let t = style_tree(&doc, &[]);
        assert_eq!(t.computed(find(&doc, "template")).display, Display::None);
    }

    #[test]
    fn ua_noscript_none() {
        // E63-M3: scripts run, so <noscript> content is not rendered.
        let doc = parse("<body><noscript><p>fallback</p></noscript></body>");
        let t = style_tree(&doc, &[]);
        assert_eq!(t.computed(find(&doc, "noscript")).display, Display::None);
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
        // `underline` line keyword honored. (E41-M1 now also honors color/style;
        // see the dedicated tests below.)
        let (doc, t) = style("<p>x</p>", "p { text-decoration: underline solid red }");
        let d = t.computed(find(&doc, "p")).text_decoration_line;
        assert!(d.contains(TextDecorationLine::UNDERLINE));
    }

    // --- E41-M1: text-decoration-color / -style ---

    #[test]
    fn text_decoration_shorthand_line_color_style() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration: underline wavy red }");
        let s = t.computed(find(&doc, "p"));
        assert!(s.text_decoration_line.contains(TextDecorationLine::UNDERLINE));
        assert_eq!(s.text_decoration_style, TextDecorationStyle::Wavy);
        assert_eq!(s.text_decoration_color, Some(red()));
    }

    #[test]
    fn text_decoration_color_longhand() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration-color: blue }");
        assert_eq!(
            t.computed(find(&doc, "p")).text_decoration_color,
            Some(blue())
        );
    }

    #[test]
    fn text_decoration_style_longhand() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration-style: dotted }");
        assert_eq!(
            t.computed(find(&doc, "p")).text_decoration_style,
            TextDecorationStyle::Dotted
        );
    }

    #[test]
    fn text_decoration_defaults() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration: underline }");
        let s = t.computed(find(&doc, "p"));
        assert_eq!(s.text_decoration_color, None); // default = element color
        assert_eq!(s.text_decoration_style, TextDecorationStyle::Solid);
    }

    // --- E41-M2: text-decoration-thickness / text-underline-offset ---

    #[test]
    fn text_decoration_thickness_length() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration-thickness: 4px }");
        assert_eq!(
            t.computed(find(&doc, "p")).text_decoration_thickness,
            Some(4.0)
        );
    }

    #[test]
    fn text_decoration_thickness_auto() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration-thickness: auto }");
        assert_eq!(t.computed(find(&doc, "p")).text_decoration_thickness, None);
    }

    #[test]
    fn text_underline_offset_length() {
        let (doc, t) = style("<p>x</p>", "p { text-underline-offset: 6px }");
        assert_eq!(t.computed(find(&doc, "p")).text_underline_offset, 6.0);
    }

    #[test]
    fn text_underline_offset_auto() {
        let (doc, t) = style("<p>x</p>", "p { text-underline-offset: auto }");
        assert_eq!(t.computed(find(&doc, "p")).text_underline_offset, 0.0);
    }

    #[test]
    fn text_decoration_thickness_offset_defaults() {
        let (doc, t) = style("<p>x</p>", "p { text-decoration: underline }");
        let s = t.computed(find(&doc, "p"));
        assert_eq!(s.text_decoration_thickness, None); // auto = derived default
        assert_eq!(s.text_underline_offset, 0.0); // auto = current behavior
    }

    // --- E41-M3: text-emphasis ---

    #[test]
    fn text_emphasis_shorthand_style_color() {
        let (doc, t) = style("<p>x</p>", "p { text-emphasis: filled dot red }");
        let s = t.computed(find(&doc, "p"));
        assert_eq!(
            s.text_emphasis.clone().map(|b| *b),
            Some(EmphasisMark {
                filled: true,
                shape: EmphasisShape::Dot
            })
        );
        assert_eq!(s.text_emphasis_color, Some(red()));
    }

    #[test]
    fn text_emphasis_style_open_circle() {
        let (doc, t) = style("<p>x</p>", "p { text-emphasis-style: open circle }");
        let s = t.computed(find(&doc, "p")).text_emphasis.clone().unwrap();
        assert!(!s.filled);
        assert_eq!(s.shape, EmphasisShape::Circle);
    }

    #[test]
    fn text_emphasis_style_string() {
        let (doc, t) = style("<p>x</p>", "p { text-emphasis-style: \"*\" }");
        assert_eq!(
            t.computed(find(&doc, "p")).text_emphasis.clone().map(|b| *b),
            Some(EmphasisMark {
                filled: true,
                shape: EmphasisShape::Str("*".into())
            })
        );
    }

    #[test]
    fn text_emphasis_style_none() {
        let (doc, t) = style("<p>x</p>", "p { text-emphasis-style: none }");
        assert_eq!(t.computed(find(&doc, "p")).text_emphasis, None);
    }

    #[test]
    fn text_emphasis_position_under() {
        let (doc, t) = style("<p>x</p>", "p { text-emphasis-position: under }");
        assert!(!t.computed(find(&doc, "p")).text_emphasis_over);
    }

    #[test]
    fn text_emphasis_position_default_over() {
        let (doc, t) = style("<p>x</p>", "p { text-emphasis: dot }");
        assert!(t.computed(find(&doc, "p")).text_emphasis_over);
    }

    #[test]
    fn text_emphasis_inherited() {
        let (doc, t) = style(
            "<p><span>x</span></p>",
            "p { text-emphasis: filled dot; text-emphasis-color: red }",
        );
        let s = t.computed(find(&doc, "span"));
        assert_eq!(
            s.text_emphasis.clone().map(|b| *b),
            Some(EmphasisMark {
                filled: true,
                shape: EmphasisShape::Dot
            })
        );
        assert_eq!(s.text_emphasis_color, Some(red()));
    }

    #[test]
    fn text_emphasis_default_absent() {
        let (doc, t) = style("<p>x</p>", "p { color: black }");
        let s = t.computed(find(&doc, "p"));
        assert_eq!(s.text_emphasis, None);
        assert_eq!(s.text_emphasis_color, None);
        assert!(s.text_emphasis_over);
    }

    // --- E51-M1: accent-color ---
    #[test]
    fn accent_color_value() {
        let (doc, t) = style("<input>", "input { accent-color: red }");
        assert_eq!(t.computed(find(&doc, "input")).accent_color, Some(red()));
    }

    #[test]
    fn accent_color_auto_is_none() {
        let (doc, t) = style("<input>", "input { accent-color: auto }");
        assert_eq!(t.computed(find(&doc, "input")).accent_color, None);
    }

    #[test]
    fn accent_color_default_none() {
        let (doc, t) = style("<input>", "input { color: black }");
        assert_eq!(t.computed(find(&doc, "input")).accent_color, None);
    }

    #[test]
    fn accent_color_inherited() {
        let (doc, t) = style("<div><input></div>", "div { accent-color: red }");
        // The child input inherits accent-color from its parent div.
        assert_eq!(t.computed(find(&doc, "input")).accent_color, Some(red()));
    }

    // --- E51-M2: appearance ---
    #[test]
    fn appearance_none_is_true() {
        let (doc, t) = style("<input>", "input { appearance: none }");
        assert!(t.computed(find(&doc, "input")).appearance_none);
    }

    #[test]
    fn appearance_auto_is_false() {
        let (doc, t) = style("<input>", "input { appearance: auto }");
        assert!(!t.computed(find(&doc, "input")).appearance_none);
    }

    #[test]
    fn appearance_default_false() {
        let (doc, t) = style("<input>", "input { color: black }");
        assert!(!t.computed(find(&doc, "input")).appearance_none);
    }

    #[test]
    fn webkit_appearance_none_is_true() {
        let (doc, t) = style("<input>", "input { -webkit-appearance: none }");
        assert!(t.computed(find(&doc, "input")).appearance_none);
    }

    #[test]
    fn appearance_not_inherited() {
        let (doc, t) = style("<div><input></div>", "div { appearance: none }");
        // appearance is NOT inherited — the child input keeps the default (auto).
        assert!(!t.computed(find(&doc, "input")).appearance_none);
    }

    // --- E51-M3: caret-color ---
    #[test]
    fn caret_color_value() {
        let (doc, t) = style("<input>", "input { caret-color: red }");
        assert_eq!(t.computed(find(&doc, "input")).caret_color, Some(red()));
    }

    #[test]
    fn caret_color_auto_is_none() {
        let (doc, t) = style("<input>", "input { caret-color: auto }");
        assert_eq!(t.computed(find(&doc, "input")).caret_color, None);
    }

    #[test]
    fn caret_color_default_none() {
        let (doc, t) = style("<input>", "input { color: black }");
        assert_eq!(t.computed(find(&doc, "input")).caret_color, None);
    }

    #[test]
    fn caret_color_inherited() {
        let (doc, t) = style("<div><input></div>", "div { caret-color: red }");
        assert_eq!(t.computed(find(&doc, "input")).caret_color, Some(red()));
    }

    // --- E51-M3: pointer-events ---
    #[test]
    fn pointer_events_none() {
        let (doc, t) = style("<div>x</div>", "div { pointer-events: none }");
        assert_eq!(
            t.computed(find(&doc, "div")).pointer_events,
            PointerEvents::None
        );
    }

    #[test]
    fn pointer_events_auto() {
        let (doc, t) = style("<div>x</div>", "div { pointer-events: auto }");
        assert_eq!(
            t.computed(find(&doc, "div")).pointer_events,
            PointerEvents::Auto
        );
    }

    #[test]
    fn pointer_events_default_auto() {
        let (doc, t) = style("<div>x</div>", "div { color: black }");
        assert_eq!(
            t.computed(find(&doc, "div")).pointer_events,
            PointerEvents::Auto
        );
    }

    #[test]
    fn pointer_events_inherited() {
        let (doc, t) = style("<div><span>x</span></div>", "div { pointer-events: none }");
        assert_eq!(
            t.computed(find(&doc, "span")).pointer_events,
            PointerEvents::None
        );
    }

    // --- E51-M3: all ---
    #[test]
    fn all_initial_resets_properties() {
        let (doc, t) = style(
            "<p>x</p>",
            "p { color: red; font-size: 30px } p { all: initial }",
        );
        let c = t.computed(find(&doc, "p"));
        // color back to initial (black), font-size back to initial (16px).
        assert_eq!(c.color, black());
        assert_eq!(c.font_size, 16.0);
    }

    #[test]
    fn all_initial_preserves_custom_props() {
        let (doc, t) = style(
            "<p>x</p>",
            "p { --x: 5px; color: red } p { all: initial; color: var(--x) }",
        );
        let c = t.computed(find(&doc, "p"));
        // The custom property survives `all: initial`; resolving var(--x) to a
        // color fails (5px isn't a color) so color stays at the reset initial.
        assert_eq!(c.color, black());
    }

    #[test]
    fn all_initial_then_later_decl_applies() {
        // A declaration AFTER `all: initial` in the same rule still applies.
        let (doc, t) = style("<p>x</p>", "p { all: initial; color: blue }");
        assert_eq!(t.computed(find(&doc, "p")).color, blue());
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

    // E42-M3: `list-style-type: <name>` resolves a registered @counter-style.
    #[test]
    fn custom_counter_style_resolved() {
        let (doc, t) = style(
            "<ul><li>a</li></ul>",
            "@counter-style box { system: cyclic; symbols: \"\u{25AA}\" } \
             ul { list-style-type: box }",
        );
        let cs = t
            .computed(find(&doc, "ul"))
            .list_style_custom
            .clone()
            .expect("custom counter style resolved");
        assert_eq!(cs.system, CounterSystem::Cyclic);
        assert_eq!(cs.format_marker(1), "\u{25AA}. ");
        // Inherits to the <li>.
        assert!(t.computed(find(&doc, "li")).list_style_custom.is_some());
    }

    #[test]
    fn custom_counter_style_numeric_counts() {
        let (doc, t) = style(
            "<ul><li>a</li></ul>",
            "@counter-style g { system: numeric; symbols: \"0\" \"1\" \"2\"; suffix: \"\" } \
             ul { list-style-type: g }",
        );
        let cs = t.computed(find(&doc, "ul")).list_style_custom.clone().unwrap();
        let got: Vec<String> = (1..=4).map(|v| cs.format_marker(v)).collect();
        assert_eq!(got, ["1", "2", "10", "11"]);
    }

    // E53-M1: `list-style-image: url(...)` / `none`, inheritance, and the
    // `list-style` shorthand picking up the image url.
    #[test]
    fn list_style_image_url_and_none() {
        let (doc, t) = style("<ul><li>a</li></ul>", "ul { list-style-image: url(d.png) }");
        assert_eq!(
            t.computed(find(&doc, "ul")).list_style_image.as_deref(),
            Some("d.png")
        );
        // Inherits to the <li>.
        assert_eq!(
            t.computed(find(&doc, "li")).list_style_image.as_deref(),
            Some("d.png")
        );

        let (doc2, t2) = style(
            "<ul><li>a</li></ul>",
            "ul { list-style-image: url(d.png) } li { list-style-image: none }",
        );
        assert_eq!(t2.computed(find(&doc2, "li")).list_style_image, None);
    }

    #[test]
    fn list_style_shorthand_sets_image() {
        let (doc, t) = style(
            "<ul><li>a</li></ul>",
            "ul { list-style: square url(d.png) }",
        );
        let s = t.computed(find(&doc, "ul"));
        assert_eq!(s.list_style_image.as_deref(), Some("d.png"));
        assert_eq!(s.list_style_type, ListStyleType::Square);
    }

    #[test]
    fn unknown_list_style_type_without_rule_unchanged() {
        // A custom name with no matching @counter-style leaves the type untouched
        // (UA default) and sets no custom style.
        let (doc, t) = style("<ul><li>a</li></ul>", "ul { list-style-type: bogus }");
        let s = t.computed(find(&doc, "ul"));
        assert_eq!(s.list_style_type, ListStyleType::Disc);
        assert!(s.list_style_custom.is_none());
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

    // E37-M1: overflow: scroll | auto parse to their own values.
    #[test]
    fn overflow_scroll_auto_values() {
        let (doc, t) = style("<p>x</p>", "p { overflow: scroll }");
        assert_eq!(t.computed(find(&doc, "p")).overflow, Overflow::Scroll);
        let (doc2, t2) = style("<p>x</p>", "p { overflow: auto }");
        assert_eq!(t2.computed(find(&doc2, "p")).overflow, Overflow::Auto);
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

    // --- E37-M3: scrollbar-width / scrollbar-color / scroll-snap ---

    #[test]
    fn scrollbar_width_values() {
        let (doc, t) = style("<p>x</p>", "p { scrollbar-width: none }");
        assert_eq!(
            t.computed(find(&doc, "p")).scrollbar_width,
            ScrollbarWidth::None
        );
        let (doc2, t2) = style("<p>x</p>", "p { scrollbar-width: thin }");
        assert_eq!(
            t2.computed(find(&doc2, "p")).scrollbar_width,
            ScrollbarWidth::Thin
        );
        let (doc3, t3) = style("<p>x</p>", "p { scrollbar-width: auto }");
        assert_eq!(
            t3.computed(find(&doc3, "p")).scrollbar_width,
            ScrollbarWidth::Auto
        );
    }

    #[test]
    fn scrollbar_width_initial_is_auto_not_inherited() {
        let (doc, t) = style("<p><span>x</span></p>", "p { scrollbar-width: none }");
        // initial Auto on a node without the property.
        assert_eq!(
            t.computed(find(&doc, "p")).scrollbar_width,
            ScrollbarWidth::None
        );
        // NOT inherited: the child stays at the initial Auto.
        assert_eq!(
            t.computed(find(&doc, "span")).scrollbar_width,
            ScrollbarWidth::Auto
        );
    }

    // --- E60-M1: scrollbar-gutter ---

    #[test]
    fn scrollbar_gutter_values() {
        let (doc, t) = style("<p>x</p>", "p { scrollbar-gutter: stable }");
        assert_eq!(
            t.computed(find(&doc, "p")).scrollbar_gutter,
            ScrollbarGutter::Stable
        );
        let (doc2, t2) = style("<p>x</p>", "p { scrollbar-gutter: stable both-edges }");
        assert_eq!(
            t2.computed(find(&doc2, "p")).scrollbar_gutter,
            ScrollbarGutter::StableBothEdges
        );
        // initial / `auto` → Auto, NOT inherited to the child.
        let (doc3, t3) = style("<p><span>x</span></p>", "p { scrollbar-gutter: auto }");
        assert_eq!(
            t3.computed(find(&doc3, "p")).scrollbar_gutter,
            ScrollbarGutter::Auto
        );
        assert_eq!(
            t3.computed(find(&doc3, "span")).scrollbar_gutter,
            ScrollbarGutter::Auto
        );
    }

    // --- E60-M2: scroll-padding / scroll-margin ---

    #[test]
    fn scroll_padding_shorthand_two_values() {
        use computed::LengthPct;
        let (doc, t) = style("<p>x</p>", "p { scroll-padding: 10px 20px }");
        // top/bottom = 10, right/left = 20 (TRBL).
        assert_eq!(
            t.computed(find(&doc, "p")).scroll_padding(),
            [
                LengthPct::Px(10.0),
                LengthPct::Px(20.0),
                LengthPct::Px(10.0),
                LengthPct::Px(20.0)
            ]
        );
    }

    #[test]
    fn scroll_margin_top_longhand() {
        let (doc, t) = style("<p>x</p>", "p { scroll-margin-top: 8px }");
        assert_eq!(t.computed(find(&doc, "p")).scroll_margin(), [8.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn scroll_padding_auto_is_zero() {
        use computed::LengthPct;
        let (doc, t) = style("<p>x</p>", "p { scroll-padding: auto }");
        assert_eq!(
            t.computed(find(&doc, "p")).scroll_padding(),
            [LengthPct::Px(0.0); 4]
        );
    }

    #[test]
    fn scroll_inset_default_none_and_not_inherited() {
        use computed::LengthPct;
        // No scroll-* declared on the child even though parent sets it: NOT inherited.
        let (doc, t) = style(
            "<p><span>x</span></p>",
            "p { scroll-padding: 5px; scroll-margin: 7px }",
        );
        let span = t.computed(find(&doc, "span"));
        assert!(span.scroll_inset.is_none());
        assert_eq!(span.scroll_padding(), [LengthPct::Px(0.0); 4]);
        assert_eq!(span.scroll_margin(), [0.0; 4]);
        // Parent did get them.
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.scroll_padding(), [LengthPct::Px(5.0); 4]);
        assert_eq!(p.scroll_margin(), [7.0; 4]);
    }

    #[test]
    fn scrollbar_color_two_colors() {
        let (doc, t) = style("<p>x</p>", "p { scrollbar-color: red blue }");
        assert_eq!(
            t.computed(find(&doc, "p")).scrollbar_color,
            Some((red(), blue()))
        );
    }

    #[test]
    fn scrollbar_color_initial_is_none() {
        let (doc, t) = style("<p>x</p>", "p { width: 10px }");
        assert_eq!(t.computed(find(&doc, "p")).scrollbar_color, None);
    }

    #[test]
    fn scroll_snap_and_behavior_parse_and_store() {
        let (doc, t) = style(
            "<p>x</p>",
            "p { scroll-snap-type: y mandatory; scroll-snap-align: center; \
                 scroll-behavior: smooth }",
        );
        let c = t.computed(find(&doc, "p"));
        let snap = c.scroll_snap.as_ref().unwrap();
        assert_eq!(snap.snap_type.as_deref(), Some("y mandatory"));
        assert_eq!(snap.snap_align.as_deref(), Some("center"));
        assert_eq!(snap.behavior.as_deref(), Some("smooth"));
    }

    // --- E22-M3: hyphens + -webkit-line-clamp ---

    #[test]
    fn hyphens_values() {
        let (doc, t) = style("<p>x</p>", "p { hyphens: none }");
        assert_eq!(t.computed(find(&doc, "p")).hyphens, Hyphens::None);
        let (doc, t) = style("<p>x</p>", "p { hyphens: manual }");
        assert_eq!(t.computed(find(&doc, "p")).hyphens, Hyphens::Manual);
        let (doc, t) = style("<p>x</p>", "p { hyphens: auto }");
        assert_eq!(t.computed(find(&doc, "p")).hyphens, Hyphens::Auto);
        // initial is manual
        let (doc, t) = style("<p>x</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).hyphens, Hyphens::Manual);
    }

    #[test]
    fn hyphens_inherited() {
        let (doc, t) = style("<p>a<span>b</span></p>", "p { hyphens: none }");
        // The child inherits hyphens from the parent.
        assert_eq!(t.computed(find(&doc, "span")).hyphens, Hyphens::None);
    }

    #[test]
    fn line_clamp_values() {
        let (doc, t) = style("<p>x</p>", "p { -webkit-line-clamp: 3 }");
        assert_eq!(t.computed(find(&doc, "p")).line_clamp, Some(3));
        let (doc, t) = style("<p>x</p>", "p { -webkit-line-clamp: none }");
        assert_eq!(t.computed(find(&doc, "p")).line_clamp, None);
        // initial is None
        let (doc, t) = style("<p>x</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).line_clamp, None);
    }

    #[test]
    fn line_clamp_not_inherited() {
        let (doc, t) = style("<p>a<span>b</span></p>", "p { -webkit-line-clamp: 2 }");
        // The child does not inherit line-clamp → stays at the initial None.
        assert_eq!(t.computed(find(&doc, "span")).line_clamp, None);
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
            (d.flex_grow, d.flex_shrink, d.flex_basis.clone()),
            (0.0, 0.0, Length::Auto)
        );

        let (doc2, t2) = style("<div>x</div>", "div { flex: auto }");
        let d2 = t2.computed(find(&doc2, "div"));
        assert_eq!(
            (d2.flex_grow, d2.flex_shrink, d2.flex_basis.clone()),
            (1.0, 1.0, Length::Auto)
        );

        // single number = grow; omitted basis defaults to 0.
        let (doc3, t3) = style("<div>x</div>", "div { flex: 1 }");
        let d3 = t3.computed(find(&doc3, "div"));
        assert_eq!(
            (d3.flex_grow, d3.flex_shrink, d3.flex_basis.clone()),
            (1.0, 1.0, Length::Px(0.0))
        );

        let (doc4, t4) = style("<div>x</div>", "div { flex: 2 3 40px }");
        let d4 = t4.computed(find(&doc4, "div"));
        assert_eq!(
            (d4.flex_grow, d4.flex_shrink, d4.flex_basis.clone()),
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
        assert_eq!(g.stops[0].pos, Some(GradientStopPos::Frac(0.0)));
        assert_eq!(g.stops[1].pos, Some(GradientStopPos::Frac(1.0)));
    }

    // E49-M2: a stop position may be a <length> (px) → `Px`, alongside `%`/auto.
    #[test]
    fn gradient_px_length_stops() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(#000 0, #000 10px, #fff 10px, #fff 20px) }",
            "div",
        );
        assert_eq!(g.stops.len(), 4);
        assert_eq!(g.stops[0].pos, Some(GradientStopPos::Frac(0.0)));
        assert_eq!(g.stops[1].pos, Some(GradientStopPos::Px(10.0)));
        assert_eq!(g.stops[2].pos, Some(GradientStopPos::Px(10.0)));
        assert_eq!(g.stops[3].pos, Some(GradientStopPos::Px(20.0)));
    }

    // E49-M2: the double-position form `color p1 p2` expands to two stops.
    #[test]
    fn gradient_double_position_stop() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(#000 10px 20px, #fff) }",
            "div",
        );
        assert_eq!(g.stops.len(), 3);
        assert_eq!(g.stops[0].color, g.stops[1].color);
        assert_eq!(g.stops[0].pos, Some(GradientStopPos::Px(10.0)));
        assert_eq!(g.stops[1].pos, Some(GradientStopPos::Px(20.0)));
        assert_eq!(g.stops[2].pos, None);
    }

    // E49-M2: `rem` lengths resolve to px against the default 16px root.
    #[test]
    fn gradient_rem_length_stop() {
        let g = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(#000 0, #fff 1rem) }",
            "div",
        );
        assert_eq!(g.stops.len(), 2);
        assert_eq!(g.stops[1].pos, Some(GradientStopPos::Px(16.0)));
    }

    // E48-M1: repeating gradients parse like their non-repeating forms but set
    // the `repeating` flag.
    #[test]
    fn repeating_linear_gradient_flag() {
        let plain = gradient(
            "<div>x</div>",
            "div { background: linear-gradient(90deg, #000, #fff) }",
            "div",
        );
        assert!(!plain.repeating, "non-repeating linear has repeating==false");

        let rep = gradient(
            "<div>x</div>",
            "div { background: repeating-linear-gradient(90deg, #000 0%, #fff 20%) }",
            "div",
        );
        assert!(rep.repeating, "repeating-linear has repeating==true");
        assert_eq!(rep.angle_deg, 90.0);
        assert_eq!(rep.stops.len(), 2);
    }

    #[test]
    fn repeating_radial_and_conic_gradient_flag() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background: repeating-radial-gradient(#000 0%, #fff 25%) }",
        );
        match &t.computed(find(&doc, "div")).background_layers[0].image {
            BgImage::Radial(g) => assert!(g.repeating, "repeating-radial flag set"),
            other => panic!("expected radial, got {other:?}"),
        }

        let (doc2, t2) = style(
            "<div>x</div>",
            "div { background: repeating-conic-gradient(#000 0%, #fff 25%) }",
        );
        match &t2.computed(find(&doc2, "div")).background_layers[0].image {
            BgImage::Conic(g) => assert!(g.repeating, "repeating-conic flag set"),
            other => panic!("expected conic, got {other:?}"),
        }
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

    // E49-M3: axis-aware `<position>` keyword parsing for background-position.
    #[test]
    fn bg_position_axis_aware() {
        let bgpos = |decl: &str| {
            let css = format!("div {{ background-image: url(a.png); background-position: {decl} }}");
            let (doc, t) = style("<div>x</div>", &css);
            t.computed(find(&doc, "div")).background_layers[0].position
        };
        // keyword pair, any order → same axis assignment.
        assert_eq!(
            bgpos("bottom right"),
            (LengthPct::Percent(100.0), LengthPct::Percent(100.0))
        );
        assert_eq!(
            bgpos("right bottom"),
            (LengthPct::Percent(100.0), LengthPct::Percent(100.0))
        );
        // `center top` → x = center, y = top.
        assert_eq!(
            bgpos("center top"),
            (LengthPct::Percent(50.0), LengthPct::Percent(0.0))
        );
        // single keyword resolves on its own axis, other = center.
        assert_eq!(
            bgpos("top"),
            (LengthPct::Percent(50.0), LengthPct::Percent(0.0))
        );
        assert_eq!(
            bgpos("left"),
            (LengthPct::Percent(0.0), LengthPct::Percent(50.0))
        );
        assert_eq!(
            bgpos("right"),
            (LengthPct::Percent(100.0), LengthPct::Percent(50.0))
        );
        assert_eq!(
            bgpos("center"),
            (LengthPct::Percent(50.0), LengthPct::Percent(50.0))
        );
        // two lengths map positionally (first = x, second = y).
        assert_eq!(bgpos("10px 20px"), (LengthPct::Px(10.0), LengthPct::Px(20.0)));
    }

    // E47-M1: background-origin / background-clip
    #[test]
    fn bg_origin_clip_default_border_box() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background-image: url(a.png) }",
        );
        let l = &t.computed(find(&doc, "div")).background_layers;
        // Engine default = border-box for byte-identity (deviates from the CSS
        // `background-origin:padding-box` initial; documented on BackgroundLayer).
        assert_eq!(l[0].origin, computed::BgGeometryBox::BorderBox);
        assert_eq!(l[0].clip, computed::BgGeometryBox::BorderBox);
    }

    // E47-M1
    #[test]
    fn bg_origin_clip_parse_per_layer() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background-image: url(a.png), url(b.png); \
             background-origin: padding-box, content-box; \
             background-clip: content-box, padding-box }",
        );
        let l = &t.computed(find(&doc, "div")).background_layers;
        assert_eq!(l[0].origin, computed::BgGeometryBox::PaddingBox);
        assert_eq!(l[1].origin, computed::BgGeometryBox::ContentBox);
        assert_eq!(l[0].clip, computed::BgGeometryBox::ContentBox);
        assert_eq!(l[1].clip, computed::BgGeometryBox::PaddingBox);
    }

    // E47-M1: a single value cycles across all layers (like the other longhands).
    #[test]
    fn bg_clip_value_cycles_across_layers() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background-image: url(a.png), url(b.png); \
             background-clip: content-box }",
        );
        let l = &t.computed(find(&doc, "div")).background_layers;
        assert_eq!(l[0].clip, computed::BgGeometryBox::ContentBox);
        assert_eq!(l[1].clip, computed::BgGeometryBox::ContentBox);
    }

    // E47-M2: background-attachment parse + store (per layer, cyclic).
    #[test]
    fn bg_attachment_parse_per_layer() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background-image: url(a.png), url(b.png); \
             background-attachment: fixed, local }",
        );
        let l = &t.computed(find(&doc, "div")).background_layers;
        assert_eq!(l[0].attachment, computed::BgAttachment::Fixed);
        assert_eq!(l[1].attachment, computed::BgAttachment::Local);
    }

    // E47-M2: default attachment is `scroll`.
    #[test]
    fn bg_attachment_default_scroll() {
        let (doc, t) = style("<div>x</div>", "div { background-image: url(a.png) }");
        let l = &t.computed(find(&doc, "div")).background_layers;
        assert_eq!(l[0].attachment, computed::BgAttachment::Scroll);
    }

    // E47-M2: background-clip: text → Text variant (color clip + layer clip).
    #[test]
    fn bg_clip_text_parse() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background-image: url(a.png); background-clip: text }",
        );
        let c = t.computed(find(&doc, "div"));
        assert_eq!(c.background_color_clip, computed::BgGeometryBox::Text);
        assert_eq!(c.background_layers[0].clip, computed::BgGeometryBox::Text);
    }

    // E47-M2: `-webkit-background-clip: text` is an alias for background-clip.
    #[test]
    fn webkit_bg_clip_text_alias() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { background-image: url(a.png); -webkit-background-clip: text }",
        );
        let c = t.computed(find(&doc, "div"));
        assert_eq!(c.background_color_clip, computed::BgGeometryBox::Text);
        assert_eq!(c.background_layers[0].clip, computed::BgGeometryBox::Text);
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

    // E50-M1
    #[test]
    fn grid_template_columns_minmax_fr() {
        use MinMaxSize as MM;
        use TrackSize::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: minmax(100px, 1fr) 1fr }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![MinMax(MM::Px(100.0), MM::Fr(1.0)), Fr(1.0)]
        );
    }

    // E50-M1
    #[test]
    fn grid_template_columns_minmax_fixed() {
        use MinMaxSize as MM;
        use TrackSize::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: minmax(50px, 80px) }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![MinMax(MM::Px(50.0), MM::Px(80.0))]
        );
    }

    // E50-M2
    #[test]
    fn grid_template_columns_intrinsic_keywords() {
        use TrackSize::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: max-content min-content }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![MaxContent, MinContent]
        );
    }

    // E50-M2
    #[test]
    fn grid_template_columns_fit_content() {
        use TrackSize::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: fit-content(60px) 1fr }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![FitContent(60.0), Fr(1.0)]
        );
    }

    // E50-M2
    #[test]
    fn grid_template_columns_minmax_intrinsic_bound() {
        use MinMaxSize as MM;
        use TrackSize::*;
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: minmax(min-content, max-content) }",
        );
        assert_eq!(
            t.computed(find(&doc, "div")).grid_template_columns,
            vec![MinMax(MM::MinContent, MM::MaxContent)]
        );
    }

    // E50-M3: auto-fill is now captured as a deferred auto-repeat (count
    // computed at layout time), with no fixed leading tracks.
    #[test]
    fn grid_template_auto_fill_parsed() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: repeat(auto-fill, 100px) }",
        );
        let c = t.computed(find(&doc, "div"));
        assert!(c.grid_template_columns.is_empty());
        assert_eq!(
            c.grid_template_columns_autorepeat,
            Some(GridAutoRepeat {
                kind: AutoRepeatKind::AutoFill,
                track: TrackSize::Px(100.0)
            })
        );
    }

    // E50-M3
    #[test]
    fn grid_template_auto_fit_parsed() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { grid-template-columns: repeat(auto-fit, 80px) }",
        );
        assert_eq!(
            t.computed(find(&doc, "div"))
                .grid_template_columns_autorepeat,
            Some(GridAutoRepeat {
                kind: AutoRepeatKind::AutoFit,
                track: TrackSize::Px(80.0)
            })
        );
    }

    // E50-M3
    #[test]
    fn grid_auto_flow_dense_flag() {
        let (doc, t) = style("<div>x</div>", "div { grid-auto-flow: row dense }");
        assert!(t.computed(find(&doc, "div")).grid_auto_flow_dense);
        let (doc2, t2) = style("<div>x</div>", "div { grid-auto-flow: row }");
        assert!(!t2.computed(find(&doc2, "div")).grid_auto_flow_dense);
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

    // E45-M2: 3D transform functions flattened to 2D `TransformFn`.
    #[test]
    fn transform_3d_flatten() {
        // rotateY(60deg) → Scale(cos60=0.5, 1).
        match xform("<div>x</div>", "div { transform: rotateY(60deg) }", "div").as_slice() {
            [TransformFn::Scale(sx, sy)] => {
                assert!((sx - 0.5).abs() < 1e-3, "sx={sx}");
                assert!((sy - 1.0).abs() < 1e-3, "sy={sy}");
            }
            o => panic!("{o:?}"),
        }
        // rotateX(60deg) → Scale(1, cos60=0.5).
        match xform("<div>x</div>", "div { transform: rotateX(60deg) }", "div").as_slice() {
            [TransformFn::Scale(sx, sy)] => {
                assert!((sx - 1.0).abs() < 1e-3, "sx={sx}");
                assert!((sy - 0.5).abs() < 1e-3, "sy={sy}");
            }
            o => panic!("{o:?}"),
        }
        // rotateZ(90deg) == rotate(90deg) → Rotate(~PI/2).
        match xform("<div>x</div>", "div { transform: rotateZ(90deg) }", "div").as_slice() {
            [TransformFn::Rotate(r)] => {
                assert!((r - std::f32::consts::FRAC_PI_2).abs() < 1e-3, "r={r}")
            }
            o => panic!("{o:?}"),
        }
        // translate3d(10px,20px,5px) → Translate(10,20).
        assert_eq!(
            xform(
                "<div>x</div>",
                "div { transform: translate3d(10px,20px,5px) }",
                "div"
            ),
            vec![TransformFn::Translate(LengthPct::Px(10.0), LengthPct::Px(20.0))]
        );
        // scale3d(2,3,4) → Scale(2,3).
        assert_eq!(
            xform("<div>x</div>", "div { transform: scale3d(2,3,4) }", "div"),
            vec![TransformFn::Scale(2.0, 3.0)]
        );
        // translateZ(5px) → identity Translate(0,0).
        assert_eq!(
            xform("<div>x</div>", "div { transform: translateZ(5px) }", "div"),
            vec![TransformFn::Translate(LengthPct::Px(0.0), LengthPct::Px(0.0))]
        );
        // scaleZ(4) → identity Scale(1,1).
        assert_eq!(
            xform("<div>x</div>", "div { transform: scaleZ(4) }", "div"),
            vec![TransformFn::Scale(1.0, 1.0)]
        );
        // perspective(500px) → identity Scale(1,1) (M2 ignores it).
        assert_eq!(
            xform("<div>x</div>", "div { transform: perspective(500px) }", "div"),
            vec![TransformFn::Scale(1.0, 1.0)]
        );
        // matrix3d(...) → 2D affine [m0,m1,m4,m5,m12,m13].
        assert_eq!(
            xform(
                "<div>x</div>",
                "div { transform: matrix3d(1,0,0,0, 0,1,0,0, 0,0,1,0, 7,8,0,1) }",
                "div"
            ),
            vec![TransformFn::Matrix([1.0, 0.0, 0.0, 1.0, 7.0, 8.0])]
        );
    }

    // E45-M2: rotate3d axis-dominant flattening.
    #[test]
    fn transform_rotate3d_axes() {
        // ~z axis → Rotate(a).
        match xform(
            "<div>x</div>",
            "div { transform: rotate3d(0,0,1,90deg) }",
            "div",
        )
        .as_slice()
        {
            [TransformFn::Rotate(r)] => {
                assert!((r - std::f32::consts::FRAC_PI_2).abs() < 1e-3, "r={r}")
            }
            o => panic!("{o:?}"),
        }
        // ~x axis → rotateX flatten: Scale(1, cos60).
        match xform(
            "<div>x</div>",
            "div { transform: rotate3d(1,0,0,60deg) }",
            "div",
        )
        .as_slice()
        {
            [TransformFn::Scale(sx, sy)] => {
                assert!((sx - 1.0).abs() < 1e-3 && (sy - 0.5).abs() < 1e-3, "{sx},{sy}")
            }
            o => panic!("{o:?}"),
        }
        // ~y axis → rotateY flatten: Scale(cos60, 1).
        match xform(
            "<div>x</div>",
            "div { transform: rotate3d(0,1,0,60deg) }",
            "div",
        )
        .as_slice()
        {
            [TransformFn::Scale(sx, sy)] => {
                assert!((sx - 0.5).abs() < 1e-3 && (sy - 1.0).abs() < 1e-3, "{sx},{sy}")
            }
            o => panic!("{o:?}"),
        }
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
        // (`perspective` now flattens to identity in E45-M2, so use only
        // genuinely-unknown functions here.)
        let (doc2, t2) = style("<div>x</div>", "div { transform: foo(1) bar(2) }");
        assert!(t2.computed(find(&doc2, "div")).transform.is_empty());
    }

    // E45-M1: individual transform properties.
    #[test]
    fn individual_translate_prop() {
        let (doc, t) = style("<div>x</div>", "div { translate: 20px 10px }");
        assert_eq!(
            t.computed(find(&doc, "div")).individual_transform.as_deref().unwrap().translate,
            Some((LengthPct::Px(20.0), LengthPct::Px(10.0)))
        );
        // y defaults to 0.
        let (doc2, t2) = style("<div>x</div>", "div { translate: 5px }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).individual_transform.as_deref().unwrap().translate,
            Some((LengthPct::Px(5.0), LengthPct::Px(0.0)))
        );
        // none → unset.
        let (doc3, t3) = style("<div>x</div>", "div { translate: none }");
        let it = t3.computed(find(&doc3, "div")).individual_transform.clone();
        assert!(it.is_none() || it.unwrap().translate.is_none());
    }

    #[test]
    fn individual_rotate_prop() {
        let (doc, t) = style("<div>x</div>", "div { rotate: 45deg }");
        match t.computed(find(&doc, "div")).individual_transform.as_deref().unwrap().rotate {
            Some(r) => assert!((r - 45.0f32.to_radians()).abs() < 1e-3, "{r}"),
            None => panic!("expected rotate"),
        }
        // axis z form keeps the angle; x/y forms flatten to 0 in M1.
        let (doc2, t2) = style("<div>x</div>", "div { rotate: z 90deg }");
        match t2.computed(find(&doc2, "div")).individual_transform.as_deref().unwrap().rotate {
            Some(r) => assert!((r - std::f32::consts::FRAC_PI_2).abs() < 1e-3, "{r}"),
            None => panic!("expected rotate"),
        }
        let (doc3, t3) = style("<div>x</div>", "div { rotate: x 90deg }");
        assert_eq!(
            t3.computed(find(&doc3, "div")).individual_transform.as_deref().unwrap().rotate,
            Some(0.0)
        );
    }

    #[test]
    fn individual_scale_prop() {
        let (doc, t) = style("<div>x</div>", "div { scale: 2 }");
        assert_eq!(
            t.computed(find(&doc, "div")).individual_transform.as_deref().unwrap().scale,
            Some((2.0, 2.0))
        );
        let (doc2, t2) = style("<div>x</div>", "div { scale: 2 3 }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).individual_transform.as_deref().unwrap().scale,
            Some((2.0, 3.0))
        );
        // percentage form.
        let (doc3, t3) = style("<div>x</div>", "div { scale: 50% }");
        assert_eq!(
            t3.computed(find(&doc3, "div")).individual_transform.as_deref().unwrap().scale,
            Some((0.5, 0.5))
        );
    }

    #[test]
    fn individual_transform_props_default_none() {
        let (doc, t) = style("<div>x</div>", "div { color: red }");
        assert!(t.computed(find(&doc, "div")).individual_transform.is_none());
    }

    // E45-M3: 3D presentation properties (perspective / backface-visibility /
    // transform-style). Only the two flags are stored; perspective is parsed and
    // ignored (accepted without error).
    #[test]
    fn backface_visibility_prop() {
        let (doc, t) = style("<div>x</div>", "div { backface-visibility: hidden }");
        assert!(t.computed(find(&doc, "div")).backface_visibility_hidden);
        let (doc2, t2) = style("<div>x</div>", "div { backface-visibility: visible }");
        assert!(!t2.computed(find(&doc2, "div")).backface_visibility_hidden);
        // default is visible (false).
        let (doc3, t3) = style("<div>x</div>", "div { color: red }");
        assert!(!t3.computed(find(&doc3, "div")).backface_visibility_hidden);
    }

    #[test]
    fn transform_style_prop() {
        let (doc, t) = style("<div>x</div>", "div { transform-style: preserve-3d }");
        assert!(t.computed(find(&doc, "div")).transform_style_preserve3d);
        let (doc2, t2) = style("<div>x</div>", "div { transform-style: flat }");
        assert!(!t2.computed(find(&doc2, "div")).transform_style_preserve3d);
        // default is flat (false).
        let (doc3, t3) = style("<div>x</div>", "div { color: red }");
        assert!(!t3.computed(find(&doc3, "div")).transform_style_preserve3d);
    }

    #[test]
    fn perspective_parses_without_error() {
        // perspective / perspective-origin are accepted (parse-and-ignore): the
        // rest of the rule still applies, so `color: red` takes effect.
        let (doc, t) = style(
            "<div>x</div>",
            "div { perspective: 500px; perspective-origin: top left; color: red }",
        );
        assert_eq!(t.computed(find(&doc, "div")).color, red());
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
        // E49-M3: axis-aware — `bottom right` now resolves x=right, y=bottom.
        let (doc5, t5) = style("<div>x</div>", "div { transform-origin: bottom right }");
        assert_eq!(
            t5.computed(find(&doc5, "div")).transform_origin,
            (LengthPct::Percent(100.0), LengthPct::Percent(100.0))
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

    // --- E64-M1: HTML `dir` attribute → CSS `direction` via UA stylesheet ---

    #[test]
    fn dir_attr_rtl_sets_direction() {
        let (doc, t) = style("<p dir=rtl>x</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).direction, Direction::Rtl);
    }

    #[test]
    fn dir_attr_ltr_sets_direction() {
        let (doc, t) = style("<p dir=ltr>x</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).direction, Direction::Ltr);
    }

    #[test]
    fn dir_attr_inherits_to_child() {
        let (doc, t) = style("<div dir=rtl><span>x</span></div>", "");
        assert_eq!(t.computed(find(&doc, "div")).direction, Direction::Rtl);
        // `direction` is inherited → the span without its own dir picks it up.
        assert_eq!(t.computed(find(&doc, "span")).direction, Direction::Rtl);
    }

    #[test]
    fn bdo_dir_rtl_forces_bidi_override() {
        let (doc, t) = style("<bdo dir=rtl>x</bdo>", "");
        let s = t.computed(find(&doc, "bdo"));
        assert_eq!(s.direction, Direction::Rtl);
        assert_eq!(s.unicode_bidi, UnicodeBidi::BidiOverride);
    }

    #[test]
    fn bdi_defaults_to_unicode_bidi_isolate() {
        // E64-M2: `<bdi>` gets `unicode-bidi: isolate` from the UA stylesheet.
        let (doc, t) = style("<bdi>x</bdi>", "");
        assert_eq!(
            t.computed(find(&doc, "bdi")).unicode_bidi,
            UnicodeBidi::Isolate
        );
    }

    #[test]
    fn no_dir_attr_keeps_initial_direction() {
        let (doc, t) = style("<p>x</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).direction, Direction::Ltr);
    }

    // --- E64-M3: `dir=auto` first-strong-character heuristic ---

    #[test]
    fn dir_auto_leading_strong_rtl() {
        let (doc, t) = style("<p dir=auto>\u{05E9}\u{05DC}\u{05D5}\u{05DD} world</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).direction, Direction::Rtl);
    }

    #[test]
    fn dir_auto_leading_strong_ltr() {
        let (doc, t) = style("<p dir=auto>hello \u{05E2}\u{05D5}\u{05DC}\u{05DD}</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).direction, Direction::Ltr);
    }

    #[test]
    fn dir_auto_no_strong_char_defaults_ltr() {
        let (doc, t) = style("<p dir=auto>123 456</p>", "");
        assert_eq!(t.computed(find(&doc, "p")).direction, Direction::Ltr);
    }

    #[test]
    fn bdi_no_dir_auto_resolves_rtl() {
        let (doc, t) = style("<bdi>\u{05E9}\u{05DC}\u{05D5}\u{05DD}</bdi>", "");
        assert_eq!(t.computed(find(&doc, "bdi")).direction, Direction::Rtl);
    }

    #[test]
    fn bdi_explicit_dir_ltr_wins_over_auto() {
        let (doc, t) = style("<bdi dir=ltr>\u{05E9}\u{05DC}\u{05D5}\u{05DD}</bdi>", "");
        assert_eq!(t.computed(find(&doc, "bdi")).direction, Direction::Ltr);
    }

    #[test]
    fn dir_auto_child_inherits_resolved_direction() {
        let (doc, t) = style("<div dir=auto>\u{05E9}\u{05DC}\u{05D5}\u{05DD}<span>x</span></div>", "");
        assert_eq!(t.computed(find(&doc, "div")).direction, Direction::Rtl);
        // child without its own dir inherits the auto-resolved direction.
        assert_eq!(t.computed(find(&doc, "span")).direction, Direction::Rtl);
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

    // E46-M1
    #[test]
    fn font_feature_settings_parses_pairs() {
        let (doc, t) = style(
            "<p>x</p>",
            r#"p { font-feature-settings: "liga" 0, "smcp" 1 }"#,
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.font_features(), &[(*b"liga", 0), (*b"smcp", 1)]);
    }

    // E46-M1: bare tag = on (1); `on`/`off` keywords map to 1/0; short tags pad.
    #[test]
    fn font_feature_settings_value_forms() {
        let (doc, t) = style(
            "<p>x</p>",
            r#"p { font-feature-settings: "kern", "liga" off, "aa" on }"#,
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(
            p.font_features(),
            &[(*b"kern", 1), (*b"liga", 0), (*b"aa  ", 1)]
        );
    }

    // E46-M1: `normal` (and the default) → no features.
    #[test]
    fn font_feature_settings_normal_is_empty() {
        let (doc, t) = style("<p>x</p>", "p { font-feature-settings: normal }");
        assert!(t.computed(find(&doc, "p")).font_features().is_empty());
        let (doc2, t2) = style("<p>x</p>", "");
        assert!(t2.computed(find(&doc2, "p")).font_features().is_empty());
        assert!(t2.computed(find(&doc2, "p")).font_feature_settings.is_none());
    }

    // E46-M1
    #[test]
    fn font_kerning_keywords() {
        let (doc, t) = style("<p>x</p>", "p { font-kerning: none }");
        assert_eq!(t.computed(find(&doc, "p")).font_kerning, FontKerning::None);
        let (doc2, t2) = style("<p>x</p>", "p { font-kerning: normal }");
        assert_eq!(t2.computed(find(&doc2, "p")).font_kerning, FontKerning::Normal);
        // default = auto.
        let (doc3, t3) = style("<p>x</p>", "");
        assert_eq!(t3.computed(find(&doc3, "p")).font_kerning, FontKerning::Auto);
    }

    // E46-M1: font-* are inherited.
    #[test]
    fn font_feature_settings_and_kerning_inherit() {
        let (doc, t) = style(
            "<div><span>x</span></div>",
            r#"div { font-feature-settings: "smcp" 1; font-kerning: none }"#,
        );
        let s = t.computed(find(&doc, "span"));
        assert_eq!(s.font_features(), &[(*b"smcp", 1)]);
        assert_eq!(s.font_kerning, FontKerning::None);
    }

    // E46-M2: font-variant-* longhands parse to the right enums.
    #[test]
    fn font_variant_longhands_parse() {
        let (doc, t) = style("<p>x</p>", "p { font-variant-caps: small-caps }");
        assert_eq!(
            t.computed(find(&doc, "p")).font_variant_caps,
            FontVariantCaps::SmallCaps
        );
        let (doc, t) = style("<p>x</p>", "p { font-variant-numeric: tabular-nums }");
        assert_eq!(
            t.computed(find(&doc, "p")).font_variant_numeric,
            FontVariantNumeric::Tabular
        );
        let (doc, t) = style(
            "<p>x</p>",
            "p { font-variant-ligatures: no-common-ligatures }",
        );
        assert_eq!(
            t.computed(find(&doc, "p")).font_variant_ligatures,
            FontVariantLigatures::NoCommon
        );
    }

    // E46-M2: variant longhands map to OpenType features in effective_font_features.
    #[test]
    fn font_variant_effective_features() {
        let (doc, t) = style("<p>x</p>", "p { font-variant-numeric: tabular-nums }");
        assert!(t
            .computed(find(&doc, "p"))
            .effective_font_features()
            .contains(&(*b"tnum", 1)));
        let (doc, t) = style(
            "<p>x</p>",
            "p { font-variant-ligatures: no-common-ligatures }",
        );
        assert!(t
            .computed(find(&doc, "p"))
            .effective_font_features()
            .contains(&(*b"liga", 0)));
        // small-caps → smcp 1.
        let (doc, t) = style("<p>x</p>", "p { font-variant-caps: small-caps }");
        assert!(t
            .computed(find(&doc, "p"))
            .effective_font_features()
            .contains(&(*b"smcp", 1)));
    }

    // E46-M2: font-feature-settings wins over font-variant on a tag conflict, and
    // it must be the LAST occurrence so rustybuzz applies it last (last-wins).
    #[test]
    fn font_variant_feature_settings_merge_order() {
        let (doc, t) = style(
            "<p>x</p>",
            r#"p { font-variant-ligatures: no-common-ligatures;
                   font-feature-settings: "liga" 1 }"#,
        );
        let eff = t.computed(find(&doc, "p")).effective_font_features();
        // variant liga 0 first, feature-settings liga 1 last → authoritative.
        let liga_positions: Vec<usize> = eff
            .iter()
            .enumerate()
            .filter(|(_, (tag, _))| tag == b"liga")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(eff[*liga_positions.last().unwrap()], (*b"liga", 1));
        assert!(eff.iter().any(|p| *p == (*b"liga", 0)));
        // feature-settings entry comes after the variant entry.
        assert!(liga_positions.last().unwrap() > liga_positions.first().unwrap());
    }

    // E46-M2: default (all Normal, no settings) → effective == empty == the
    // borrowed font_features slice (byte-identical shaping input).
    #[test]
    fn font_variant_default_effective_empty() {
        let (doc, t) = style("<p>x</p>", "");
        let p = t.computed(find(&doc, "p"));
        assert!(p.effective_font_features().is_empty());
        assert_eq!(p.effective_font_features().as_slice(), p.font_features());
    }

    // E46-M2: `font-variant` shorthand (MVP). small-caps sets caps; normal resets.
    #[test]
    fn font_variant_shorthand() {
        let (doc, t) = style("<p>x</p>", "p { font-variant: small-caps }");
        assert_eq!(
            t.computed(find(&doc, "p")).font_variant_caps,
            FontVariantCaps::SmallCaps
        );
        let (doc, t) = style(
            "<div><p>x</p></div>",
            "div { font-variant: small-caps } p { font-variant: normal }",
        );
        assert_eq!(
            t.computed(find(&doc, "p")).font_variant_caps,
            FontVariantCaps::Normal
        );
    }

    // E46-M2: font-variant-* are inherited.
    #[test]
    fn font_variant_inherits() {
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { font-variant-caps: small-caps; font-variant-numeric: tabular-nums }",
        );
        let s = t.computed(find(&doc, "span"));
        assert_eq!(s.font_variant_caps, FontVariantCaps::SmallCaps);
        assert_eq!(s.font_variant_numeric, FontVariantNumeric::Tabular);
    }

    // E46-M3: font-variation-settings parses to (axis, coord) pairs; negative
    // coords are kept (e.g. `slnt` -10).
    #[test]
    fn font_variation_settings_parses_pairs() {
        let (doc, t) = style(
            "<p>x</p>",
            r#"p { font-variation-settings: "wght" 700, "slnt" -10 }"#,
        );
        let p = t.computed(find(&doc, "p"));
        assert_eq!(p.font_variations(), &[(*b"wght", 700.0), (*b"slnt", -10.0)]);
    }

    // E46-M3: `normal` (and the default) → no variation axes.
    #[test]
    fn font_variation_settings_normal_is_empty() {
        let (doc, t) = style("<p>x</p>", "p { font-variation-settings: normal }");
        let p = t.computed(find(&doc, "p"));
        assert!(p.font_variations().is_empty());
        assert!(p.font_variation_settings.is_none());
        let (doc2, t2) = style("<p>x</p>", "");
        assert!(t2.computed(find(&doc2, "p")).font_variation_settings.is_none());
    }

    // E46-M3: font-variation-settings is inherited.
    #[test]
    fn font_variation_settings_inherits() {
        let (doc, t) = style(
            "<div><span>x</span></div>",
            r#"div { font-variation-settings: "wght" 600 }"#,
        );
        let s = t.computed(find(&doc, "span"));
        assert_eq!(s.font_variations(), &[(*b"wght", 600.0)]);
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

    // E53-M2: `content: url(...)` on a pseudo → a Url content (image pseudo).
    #[test]
    fn pseudo_content_url_entry() {
        let (doc, t) = style(
            "<div>x</div>",
            "div::before { content: url(i.png) }",
        );
        let (s, text) = t
            .pseudo(find(&doc, "div"), PseudoElement::Before)
            .expect("before entry");
        assert_eq!(s.content, Content::Url("i.png".to_string()));
        // The url is carried in the content string for the box tree.
        assert_eq!(text, "i.png");
    }

    // E53-M2: a quoted url is stripped of its quotes.
    #[test]
    fn pseudo_content_url_quoted() {
        let (doc, t) = style(
            "<div>x</div>",
            "div::before { content: url(\"a/b.png\") }",
        );
        let (s, _) = t
            .pseudo(find(&doc, "div"), PseudoElement::Before)
            .expect("before entry");
        assert_eq!(s.content, Content::Url("a/b.png".to_string()));
    }

    // E53-M2: `content: none` → no generated pseudo (verify the existing rule).
    #[test]
    fn pseudo_content_none_no_entry() {
        let (doc, t) = style("<div>x</div>", "div::before { content: none }");
        assert!(t.pseudo(find(&doc, "div"), PseudoElement::Before).is_none());
    }

    // E53-M2: a mixed `"[" attr(data-y) "]"` list flattens its text parts in order.
    #[test]
    fn pseudo_content_mixed_order() {
        let (doc, t) = style(
            "<div data-y='Z'>x</div>",
            "[data-y]::before { content: \"[\" attr(data-y) \"]\" }",
        );
        let (s, text) = t
            .pseudo(find(&doc, "div"), PseudoElement::Before)
            .expect("before entry");
        assert_eq!(text, "[Z]");
        assert_eq!(s.content, Content::Text("[Z]".to_string()));
    }

    // --- E53-M3: `quotes` + open-quote/close-quote ---

    // `quotes: "«" "»"` parses to one (open, close) pair.
    #[test]
    fn quotes_one_pair_parses() {
        let (doc, t) = style("<p>x</p>", "p { quotes: \"\u{ab}\" \"\u{bb}\" }");
        let q = t.computed(find(&doc, "p")).quotes.as_ref().expect("pairs");
        assert_eq!(q.as_slice(), &[("\u{ab}".to_string(), "\u{bb}".to_string())]);
    }

    // `quotes: none` → the "none" representation (Some(empty Vec)).
    #[test]
    fn quotes_none_is_empty_pairs() {
        let (doc, t) = style("<p>x</p>", "p { quotes: none }");
        let q = t.computed(find(&doc, "p")).quotes.as_ref().expect("some");
        assert!(q.is_empty());
    }

    // `content: open-quote` parses to the `OpenQuote` item (then resolves to a
    // mark on the pseudo, but the parser classifies it as OpenQuote first).
    #[test]
    fn content_open_quote_parses() {
        use crate::properties::resolve_content;
        let doc = parse("<p>x</p>");
        let p = find(&doc, "p");
        let sheet = parse_stylesheet("p::before { content: open-quote }");
        let decl = &sheet.rules[0].declarations[0];
        let cs = crate::counters::CounterState::default();
        assert_eq!(resolve_content(&doc, p, decl, &cs), Content::OpenQuote);
    }

    // A `<q>` with `q{quotes:'«' '»'}`: its ::before resolves to "«", ::after "»".
    #[test]
    fn q_before_after_use_quotes() {
        let (doc, t) = style(
            "<q>hi</q>",
            "q { quotes: \"\u{ab}\" \"\u{bb}\" }",
        );
        let q = find(&doc, "q");
        let (_, before) = t.pseudo(q, PseudoElement::Before).expect("before");
        let (_, after) = t.pseudo(q, PseudoElement::After).expect("after");
        assert_eq!(before, "\u{ab}");
        assert_eq!(after, "\u{bb}");
    }

    // Nested `<q><q></q></q>` with two pairs: the inner ::before uses the
    // LEVEL-2 open mark, distinct from the outer's level-1 mark.
    #[test]
    fn nested_q_uses_next_level() {
        let (doc, t) = style(
            "<q id='o'>a<q id='i'>b</q>c</q>",
            "q { quotes: \"L1o\" \"L1c\" \"L2o\" \"L2c\" }",
        );
        let outer = find_id(&doc, "o");
        let inner = find_id(&doc, "i");
        let (_, o_before) = t.pseudo(outer, PseudoElement::Before).expect("o::before");
        let (_, i_before) = t.pseudo(inner, PseudoElement::Before).expect("i::before");
        assert_eq!(o_before, "L1o", "outer uses level 1");
        assert_eq!(i_before, "L2o", "inner uses level 2");
        assert_ne!(o_before, i_before);
        // close-quote pops back: outer ::after uses the level-1 close mark.
        let (_, o_after) = t.pseudo(outer, PseudoElement::After).expect("o::after");
        assert_eq!(o_after, "L1c");
    }

    // `quotes: none` → open-quote emits an empty mark.
    #[test]
    fn quotes_none_open_quote_empty() {
        let (doc, t) = style("<q>x</q>", "q { quotes: none }");
        let (_, before) = t
            .pseudo(find(&doc, "q"), PseudoElement::Before)
            .expect("before");
        assert_eq!(before, "");
    }

    // The UA default (no author `quotes`) auto-quotes `<q>` with ASCII marks.
    #[test]
    fn q_default_quotes_from_ua() {
        let (doc, t) = style("<q>x</q>", "");
        let (_, before) = t
            .pseudo(find(&doc, "q"), PseudoElement::Before)
            .expect("before");
        assert_eq!(before, "\"");
    }

    // --- E35-M1: ::marker pseudo cascade ---

    #[test]
    fn marker_color_only_entry() {
        // `li::marker{color:red}` → a marker pseudo style (red), empty content.
        let (doc, t) = style(
            "<ul><li>x</li></ul>",
            "li::marker { color: red }",
        );
        let (s, text) = t
            .pseudo(find(&doc, "li"), PseudoElement::Marker)
            .expect("marker entry");
        assert_eq!(s.color, red());
        assert_eq!(text, "");
    }

    #[test]
    fn marker_content_entry() {
        // `::marker{content:"X "}` → content string carried for text replacement.
        let (doc, t) = style(
            "<ul><li>x</li></ul>",
            "li::marker { content: \"X \" }",
        );
        let (_, text) = t
            .pseudo(find(&doc, "li"), PseudoElement::Marker)
            .expect("marker entry");
        assert_eq!(text, "X ");
    }

    #[test]
    fn marker_no_rule_no_entry() {
        let (doc, t) = style("<ul><li>x</li></ul>", "li { color: red }");
        assert!(t
            .pseudo(find(&doc, "li"), PseudoElement::Marker)
            .is_none());
    }

    // --- E35-M2: ::placeholder pseudo cascade ---

    #[test]
    fn placeholder_color_only_entry() {
        // `input::placeholder{color:#06c}` → a placeholder pseudo style, empty content.
        let (doc, t) = style(
            "<input placeholder='hi'>",
            "input::placeholder { color: #06c }",
        );
        let (s, text) = t
            .pseudo(find(&doc, "input"), PseudoElement::Placeholder)
            .expect("placeholder entry");
        assert_eq!(
            s.color,
            Rgba {
                r: 0,
                g: 0x66,
                b: 0xcc,
                a: 255
            }
        );
        assert_eq!(text, "");
    }

    #[test]
    fn placeholder_no_rule_no_entry() {
        let (doc, t) = style("<input placeholder='hi'>", "input { color: red }");
        assert!(t
            .pseudo(find(&doc, "input"), PseudoElement::Placeholder)
            .is_none());
    }

    // --- E35-M3: ::first-letter pseudo cascade ---

    #[test]
    fn first_letter_color_only_entry() {
        // `p::first-letter{color:#c00}` → a first-letter pseudo style (red), empty content.
        let (doc, t) = style("<p>Hello</p>", "p::first-letter { color: #c00 }");
        let (s, text) = t
            .pseudo(find(&doc, "p"), PseudoElement::FirstLetter)
            .expect("first-letter entry");
        assert_eq!(
            s.color,
            Rgba {
                r: 0xcc,
                g: 0,
                b: 0,
                a: 255
            }
        );
        assert_eq!(text, "");
    }

    #[test]
    fn first_letter_no_rule_no_entry() {
        let (doc, t) = style("<p>Hello</p>", "p { color: red }");
        assert!(t
            .pseudo(find(&doc, "p"), PseudoElement::FirstLetter)
            .is_none());
    }

    // --- E56-M2: initial-letter (drop cap) ---

    #[test]
    fn initial_letter_size_only() {
        // `initial-letter: 3` on ::first-letter → Some(3.0).
        let (doc, t) = style("<p>Hello</p>", "p::first-letter { initial-letter: 3 }");
        let (s, _) = t
            .pseudo(find(&doc, "p"), PseudoElement::FirstLetter)
            .expect("first-letter entry");
        assert_eq!(s.initial_letter, Some(3.0));
    }

    #[test]
    fn initial_letter_size_and_sink() {
        // `initial-letter: 3 2` → size 3 (sink ignored, MVP).
        let (doc, t) = style("<p>Hello</p>", "p::first-letter { initial-letter: 3 2 }");
        let (s, _) = t
            .pseudo(find(&doc, "p"), PseudoElement::FirstLetter)
            .expect("first-letter entry");
        assert_eq!(s.initial_letter, Some(3.0));
    }

    #[test]
    fn initial_letter_normal_is_none() {
        let (doc, t) = style(
            "<p>Hello</p>",
            "p::first-letter { initial-letter: normal }",
        );
        let (s, _) = t
            .pseudo(find(&doc, "p"), PseudoElement::FirstLetter)
            .expect("first-letter entry");
        assert_eq!(s.initial_letter, None);
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

    // E40-M2
    #[test]
    fn table_layout_values() {
        let (doc, t) = style("<div>x</div>", "div { table-layout: fixed }");
        assert_eq!(
            t.computed(find(&doc, "div")).table_layout,
            TableLayout::Fixed
        );
        // default Auto.
        let (doc2, t2) = style("<div>x</div>", "div { color: red }");
        assert_eq!(
            t2.computed(find(&doc2, "div")).table_layout,
            TableLayout::Auto
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

    // --- E24-M1: min/max/clamp/round/mod/rem ---

    #[test]
    fn clamp_width_mixing_px_percent_survives_as_math() {
        let (doc, t) = style("<p>x</p>", "p { width: clamp(200px, 50%, 600px) }");
        match &t.computed(find(&doc, "p")).width {
            Length::Math(m) => {
                assert_eq!(m.resolve(300.0), 200.0);
                assert_eq!(m.resolve(1000.0), 500.0);
                assert_eq!(m.resolve(2000.0), 600.0);
            }
            other => panic!("expected Length::Math, got {other:?}"),
        }
    }

    #[test]
    fn min_pure_px_folds_to_px() {
        let (doc, t) = style("<p>x</p>", "p { width: min(100px, 200px) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Px(100.0));
    }

    #[test]
    fn min_pure_percent_folds_to_percent() {
        let (doc, t) = style("<p>x</p>", "p { width: min(10%, 20%) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Percent(10.0));
    }

    #[test]
    fn font_size_clamp_folds_to_px() {
        // parent (body) font-size 16 → clamp(12px, 50%, 20px) = max(12, min(8, 20)) = 12.
        let (doc, t) = style("<p>x</p>", "p { font-size: clamp(12px, 50%, 20px) }");
        assert_eq!(t.computed(find(&doc, "p")).font_size, 12.0);
    }

    #[test]
    fn round_folds_to_px() {
        let (doc, t) = style("<p>x</p>", "p { width: round(105px, 10px) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Px(110.0));
    }

    #[test]
    fn round_unsupported_strategy_is_noop() {
        let (doc, t) = style("<p>x</p>", "p { width: round(up, 105px, 10px) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Auto);
    }

    #[test]
    fn mod_with_percent_is_noop() {
        let (doc, t) = style("<p>x</p>", "p { width: mod(50%, 10px) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Auto);
    }

    #[test]
    fn clamp_bad_arity_is_noop() {
        let (doc, t) = style("<p>x</p>", "p { width: clamp(1px, 2px) }");
        assert_eq!(t.computed(find(&doc, "p")).width, Length::Auto);
    }

    #[test]
    fn min_in_px_only_context_requires_pure_px() {
        // border-width can't take a %: min(2px, 4px) folds to 2px, while
        // min(2px, 1%) is dropped (width left at the initial 0).
        let (doc, t) = style(
            "<p>x</p>",
            "p { border-style: solid; border-width: min(2px, 4px) }",
        );
        assert_eq!(t.computed(find(&doc, "p")).border_top_width, 2.0);
        let (doc, t) = style(
            "<p>x</p>",
            "p { border-style: solid; border-width: min(2px, 1%) }",
        );
        assert_eq!(t.computed(find(&doc, "p")).border_top_width, 0.0);
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
            ..Default::default()
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

    // --- E24-M2: @supports ---

    #[test]
    fn supports_supported_decl_applies() {
        let css = "@supports (display: grid) { p { color: blue } }";
        let (d, t) = style("<p>x</p>", css);
        assert_eq!(t.computed(find(&d, "p")).color, blue());
    }

    #[test]
    fn supports_unsupported_decl_does_not_apply() {
        let css = "@supports (display: nonsense) { p { color: blue } }";
        let (d, t) = style("<p>x</p>", css);
        assert_eq!(t.computed(find(&d, "p")).color, black());
    }

    #[test]
    fn supports_not_and_or() {
        // not (unsupported) → matches.
        let (d, t) = style(
            "<p>x</p>",
            "@supports not (display: nonsense) { p { color: blue } }",
        );
        assert_eq!(t.computed(find(&d, "p")).color, blue());
        // and: one unsupported branch fails the whole condition.
        let (d2, t2) = style(
            "<p>x</p>",
            "@supports (display: block) and (display: nonsense) { p { color: blue } }",
        );
        assert_eq!(t2.computed(find(&d2, "p")).color, black());
        // or: one supported branch suffices.
        let (d3, t3) = style(
            "<p>x</p>",
            "@supports (display: nonsense) or (width: 10px) { p { color: blue } }",
        );
        assert_eq!(t3.computed(find(&d3, "p")).color, blue());
    }

    #[test]
    fn supports_source_order_interleave() {
        // A matching @supports block sorts at its source position: the later
        // top-level rule wins the tie.
        let css = "@supports (width: 10px) { p { color: blue } } p { color: green }";
        let (d, t) = style("<p>x</p>", css);
        assert_eq!(t.computed(find(&d, "p")).color, green());
    }

    #[test]
    fn media_and_supports_adjacent_at_ordinal_order() {
        // Two adjacent at-blocks share source_index 0; at_ordinal keeps their
        // source order, so the LATER block wins the tie — in both arrangements.
        let css = "@media (min-width:100px) { p { color: red } } \
                   @supports (width: 10px) { p { color: blue } }";
        let (d, t) = style_vp("<p>x</p>", css, Viewport::from_width(800.0));
        assert_eq!(t.computed(find(&d, "p")).color, blue());

        let css2 = "@supports (width: 10px) { p { color: blue } } \
                    @media (min-width:100px) { p { color: red } }";
        let (d2, t2) = style_vp("<p>x</p>", css2, Viewport::from_width(800.0));
        assert_eq!(t2.computed(find(&d2, "p")).color, red());
    }

    // --- E24-M2: @layer ---

    #[test]
    fn layer_order_beats_source_order() {
        // `@layer base, theme;` declares theme later → theme wins even though
        // its block appears BEFORE base's in source.
        let css = "@layer base, theme; \
                   @layer theme { p { color: blue } } \
                   @layer base { p { color: red } }";
        let (d, t) = style("<p>x</p>", css);
        assert_eq!(t.computed(find(&d, "p")).color, blue());
    }

    #[test]
    fn unlayered_beats_layered() {
        // Unlayered author styles beat any layered style (normal declarations),
        // regardless of source position.
        let css = "p { color: green } @layer base { p { color: red } }";
        let (d, t) = style("<p>x</p>", css);
        assert_eq!(t.computed(find(&d, "p")).color, green());
        let css2 = "@layer base { p { color: red } } p { color: green }";
        let (d2, t2) = style("<p>x</p>", css2);
        assert_eq!(t2.computed(find(&d2, "p")).color, green());
    }

    #[test]
    fn important_inverts_layer_order() {
        // Among !important declarations EARLIER layers win: base beats theme.
        let css = "@layer base, theme; \
                   @layer base { p { color: red !important } } \
                   @layer theme { p { color: blue !important } }";
        let (d, t) = style("<p>x</p>", css);
        assert_eq!(t.computed(find(&d, "p")).color, red());
    }

    #[test]
    fn layered_important_beats_unlayered_important() {
        let css = "@layer base { p { color: red !important } } \
                   p { color: blue !important }";
        let (d, t) = style("<p>x</p>", css);
        assert_eq!(t.computed(find(&d, "p")).color, red());
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
    fn linear_points_eval_interpolates() {
        // E59-M2: piecewise-linear through (0,0),(0.25,0.5),(1,1).
        let lp = Easing::LinearPoints(
            vec![(0.0, 0.0), (0.25, 0.5), (1.0, 1.0)].into_boxed_slice(),
        );
        // At the explicit control point input 0.25 → output 0.5.
        assert!((lp.eval(0.25) - 0.5).abs() < 1e-6, "{}", lp.eval(0.25));
        // Midway up the first segment: input 0.125 → 0.25.
        assert!((lp.eval(0.125) - 0.25).abs() < 1e-6, "{}", lp.eval(0.125));
        // Endpoints and out-of-range clamp to the first/last output.
        assert!((lp.eval(0.0) - 0.0).abs() < 1e-6);
        assert!((lp.eval(1.0) - 1.0).abs() < 1e-6);
        assert!((lp.eval(-1.0) - 0.0).abs() < 1e-6);
        assert!((lp.eval(2.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn steps_jump_terms_endpoints() {
        // E59-M2: each jump term at t=0 / t→1.
        let end = Easing::Steps(4, JumpTerm::End);
        assert_eq!(end.eval(0.0), 0.0);
        assert_eq!(end.eval(1.0), 1.0);
        // jump-start jumps immediately: t=0 → 1/4.
        let start = Easing::Steps(4, JumpTerm::Start);
        assert!((start.eval(0.0) - 0.25).abs() < 1e-6, "{}", start.eval(0.0));
        assert_eq!(start.eval(1.0), 1.0);
        // jump-both: a step at both ends. At t=0 it is 1/(n+1), and it differs
        // from jump-end at both endpoints.
        let both = Easing::Steps(4, JumpTerm::Both);
        assert!((both.eval(0.0) - 0.2).abs() < 1e-6, "{}", both.eval(0.0));
        assert_eq!(both.eval(1.0), 1.0);
        assert_ne!(both.eval(0.0), end.eval(0.0));
        assert_ne!(both.eval(0.99), end.eval(0.99));
        // jump-none: no step at either end; t=0 → 0, last value reached only at 1.
        let none = Easing::Steps(4, JumpTerm::None);
        assert_eq!(none.eval(0.0), 0.0);
        assert_eq!(none.eval(1.0), 1.0);
        // Over n=4 with no end jumps, the interior step at t in [0.5,0.75) is
        // floor(t*4)/(4-1) = 2/3.
        assert!((none.eval(0.6) - 2.0 / 3.0).abs() < 1e-6, "{}", none.eval(0.6));
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

    // --- E59-M1: multiple animations ---

    #[test]
    fn anim_name_comma_list_two_animations() {
        // E59-M1: `animation-name: a, b` → two animations.
        let (d, t) = style("<div>x</div>", "div { animation-name: a, b }");
        let anims = &t.computed(find(&d, "div")).animation;
        assert_eq!(anims.len(), 2);
        assert_eq!(anims[0].name, "a");
        assert_eq!(anims[1].name, "b");
    }

    #[test]
    fn anim_shorthand_comma_list_durations() {
        // E59-M1: `animation: spin 1s, fade 2s linear` → two with right names/durations.
        let (d, t) = style(
            "<div>x</div>",
            "div { animation: spin 1s, fade 2s linear }",
        );
        let anims = &t.computed(find(&d, "div")).animation;
        assert_eq!(anims.len(), 2);
        assert_eq!(anims[0].name, "spin");
        assert!((anims[0].duration_s - 1.0).abs() < 1e-6);
        assert_eq!(anims[1].name, "fade");
        assert!((anims[1].duration_s - 2.0).abs() < 1e-6);
        assert_eq!(anims[1].timing, Easing::Linear);
    }

    #[test]
    fn anim_duration_cycles_over_names() {
        // E59-M1: two names, one duration → the duration cycles to both.
        let (d, t) = style(
            "<div>x</div>",
            "div { animation-name: a, b; animation-duration: 1s }",
        );
        let anims = &t.computed(find(&d, "div")).animation;
        assert_eq!(anims.len(), 2);
        assert!((anims[0].duration_s - 1.0).abs() < 1e-6);
        assert!((anims[1].duration_s - 1.0).abs() < 1e-6);
    }

    #[test]
    fn anim_sample_two_props_both_applied() {
        // E59-M1: one animation drives opacity, another drives color; at --at BOTH
        // are sampled.
        let css = "div { animation: fade 10s linear, recolor 10s linear } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } } \
                   @keyframes recolor { from { color: #000000 } to { color: #ffffff } }";
        let (d, t) = style_at("<div>x</div>", css, 5.0);
        let cs = t.computed(find(&d, "div"));
        assert!((cs.opacity - 0.5).abs() < 1e-3);
        assert_eq!((cs.color.r, cs.color.g, cs.color.b), (128, 128, 128));
    }

    #[test]
    fn anim_sample_same_prop_later_wins() {
        // E59-M1: two animations both drive opacity; the later one wins.
        let css = "div { animation: a 10s linear, b 10s linear } \
                   @keyframes a { from { opacity: 0 } to { opacity: 0.4 } } \
                   @keyframes b { from { opacity: 0 } to { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 10.0);
        // `a` would give 0.4, `b` gives 1.0; later (`b`) wins.
        assert_eq!(t.computed(find(&d, "div")).opacity, 1.0);
    }

    #[test]
    fn anim_single_byte_identical() {
        // E59-M1: a single animation is a Vec of length 1, sampled exactly as the
        // E17 Option path.
        let css = "div { animation: fade 10s linear } \
                   @keyframes fade { from { opacity: 0 } to { opacity: 1 } }";
        let (d, t) = style_at("<div>x</div>", css, 5.0);
        let cs = t.computed(find(&d, "div"));
        assert_eq!(cs.animation.len(), 1);
        assert!((cs.opacity - 0.5).abs() < 1e-3);
    }

    #[test]
    fn easing_linear_points_parses() {
        // E59-M2: `linear(0, 0.5 25%, 1)` → LinearPoints with the right points.
        let (d, t) = style(
            "<div>x</div>",
            "div { animation: a 1s linear(0, 0.5 25%, 1) }",
        );
        let anims = &t.computed(find(&d, "div")).animation;
        match &anims[0].timing {
            Easing::LinearPoints(pts) => {
                assert_eq!(pts.len(), 3);
                assert!((pts[0].0 - 0.0).abs() < 1e-6 && (pts[0].1 - 0.0).abs() < 1e-6);
                assert!((pts[1].0 - 0.25).abs() < 1e-6 && (pts[1].1 - 0.5).abs() < 1e-6);
                assert!((pts[2].0 - 1.0).abs() < 1e-6 && (pts[2].1 - 1.0).abs() < 1e-6);
            }
            other => panic!("expected LinearPoints, got {other:?}"),
        }
    }

    #[test]
    fn easing_steps_jump_keywords_parse() {
        // E59-M2: jump-both / jump-none / start alias.
        let (d, t) = style(
            "<div>x</div>",
            "div { animation-timing-function: steps(4, jump-both), steps(3, jump-none), steps(2, start) }",
        );
        let anims = &t.computed(find(&d, "div")).animation;
        assert_eq!(anims[0].timing, Easing::Steps(4, JumpTerm::Both));
        assert_eq!(anims[1].timing, Easing::Steps(3, JumpTerm::None));
        assert_eq!(anims[2].timing, Easing::Steps(2, JumpTerm::Start));
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

    // --- E59-M3: animations / transitions on ::before / ::after pseudos ---

    #[test]
    fn pseudo_before_animation_samples_opacity() {
        // A `::before` with an animation is sampled at `--at` just like an element.
        let css = "p::before { content: 'x'; animation: fade 1s linear } \
                   @keyframes fade { from { opacity: 1 } to { opacity: 0 } }";
        let before = |doc: &Document, t: &StyledTree| {
            t.pseudo(find(doc, "p"), PseudoElement::Before)
                .expect("::before entry")
                .0
                .opacity
        };
        // t=0 → 1 (the `from` frame).
        let (d0, t0) = style_at("<p>y</p>", css, 0.0);
        assert_eq!(before(&d0, &t0), 1.0);
        // t=0.5 → 0.5 (linear midpoint).
        let (d5, t5) = style_at("<p>y</p>", css, 0.5);
        assert!((before(&d5, &t5) - 0.5).abs() < 1e-3, "{}", before(&d5, &t5));
    }

    #[test]
    fn pseudo_after_animation_samples_opacity() {
        // `::after` (resolved after the subtree) is sampled identically.
        let css = "p::after { content: 'x'; animation: fade 1s linear } \
                   @keyframes fade { from { opacity: 1 } to { opacity: 0 } }";
        let (d, t) = style_at("<p>y</p>", css, 0.5);
        let op = t
            .pseudo(find(&d, "p"), PseudoElement::After)
            .expect("::after entry")
            .0
            .opacity;
        assert!((op - 0.5).abs() < 1e-3, "{op}");
    }

    #[test]
    fn pseudo_no_animation_untouched() {
        // A `::before` without an animation keeps its cascaded opacity (1.0), even
        // when an unrelated `@keyframes` exists in the sheet.
        let css = "p::before { content: 'x' } \
                   @keyframes fade { from { opacity: 1 } to { opacity: 0 } }";
        let (d, t) = style_at("<p>y</p>", css, 0.5);
        let op = t
            .pseudo(find(&d, "p"), PseudoElement::Before)
            .expect("::before entry")
            .0
            .opacity;
        assert_eq!(op, 1.0);
    }

    #[test]
    fn pseudo_before_transition_samples_from_to() {
        // A `::before` transition interpolates from the pre-script pseudo style to
        // the current one. Build `from`/`to` trees and bump the ::before opacity.
        let css = "p::before { content: 'x'; opacity: 1; transition: opacity 10s linear }";
        let doc = parse("<p>y</p>");
        let sheet = parse_stylesheet(css);
        let vp = Viewport::from_width(800.0);
        let from_tree = style_tree_vp(&doc, std::slice::from_ref(&sheet), vp);
        let id = find(&doc, "p");

        let build_to = || {
            let mut tree = style_tree_vp(&doc, std::slice::from_ref(&sheet), vp);
            // Script lowers the ::before opacity to 0.
            tree.before.get_mut(&id).unwrap().0.opacity = 0.0;
            tree
        };
        let op = |t: &StyledTree| t.pseudo(id, PseudoElement::Before).unwrap().0.opacity;

        // p=0 → from (1.0).
        let mut t0 = build_to();
        apply_transitions(&doc, &from_tree, &mut t0, 0.0, vp);
        assert_eq!(op(&t0), 1.0);
        // p=0.5 → 0.5.
        let mut t5 = build_to();
        apply_transitions(&doc, &from_tree, &mut t5, 5.0, vp);
        assert!((op(&t5) - 0.5).abs() < 1e-3, "{}", op(&t5));
        // p=1 → to (0.0).
        let mut t10 = build_to();
        apply_transitions(&doc, &from_tree, &mut t10, 10.0, vp);
        assert_eq!(op(&t10), 0.0);
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

    // --- E32-M3: isolation ---

    use computed::Isolation;

    #[test]
    fn isolation_isolate() {
        let (doc, t) = style("<div>x</div>", "div { isolation: isolate }");
        assert_eq!(t.computed(find(&doc, "div")).isolation, Isolation::Isolate);
    }

    #[test]
    fn isolation_auto() {
        let (doc, t) = style("<div>x</div>", "div { isolation: auto }");
        assert_eq!(t.computed(find(&doc, "div")).isolation, Isolation::Auto);
    }

    #[test]
    fn isolation_initial_auto() {
        let (doc, t) = style("<div>x</div>", "div {}");
        assert_eq!(t.computed(find(&doc, "div")).isolation, Isolation::Auto);
    }

    // --- E21-M3: mask-image + backdrop-filter ---

    use computed::{BgRepeat, MaskGeometryBox, MaskImage, MaskMode};

    #[test]
    fn mask_image_gradient_parses() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask-image: linear-gradient(black, rgba(0,0,0,0)) }",
        );
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask.len(), 1);
        let m = &mask[0];
        assert!(matches!(m.image, MaskImage::Gradient(_)));
        assert_eq!(m.mode, MaskMode::Alpha); // initial
    }

    #[test]
    fn mask_mode_luminance_parses() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask-image: linear-gradient(black, white); mask-mode: luminance }",
        );
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask[0].mode, MaskMode::Luminance);
    }

    #[test]
    fn mask_origin_clip_default_border_box() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask-image: linear-gradient(black, white) }",
        );
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask[0].origin, MaskGeometryBox::BorderBox); // initial
        assert_eq!(mask[0].clip, MaskGeometryBox::BorderBox); // initial
    }

    #[test]
    fn mask_origin_clip_parses() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask-image: linear-gradient(black, white); \
             mask-origin: content-box; mask-clip: padding-box }",
        );
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask[0].origin, MaskGeometryBox::ContentBox);
        assert_eq!(mask[0].clip, MaskGeometryBox::PaddingBox);
    }

    #[test]
    fn mask_none_clears() {
        let (doc, t) = style("<div>x</div>", "div { mask-image: none }");
        assert!(t.computed(find(&doc, "div")).mask.is_empty());
    }

    #[test]
    fn mask_not_inherited() {
        let (doc, t) = style(
            "<div><span>x</span></div>",
            "div { mask-image: linear-gradient(black, rgba(0,0,0,0)) }",
        );
        assert!(t.computed(find(&doc, "span")).mask.is_empty());
    }

    // --- E47-M3: multi-layer masks + `mask` shorthand ---

    #[test]
    fn mask_image_comma_list_two_layers() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask-image: linear-gradient(black, white), url(x.png) }",
        );
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask.len(), 2, "two comma-separated layers");
        assert!(matches!(mask[0].image, MaskImage::Gradient(_)));
        assert!(matches!(mask[1].image, MaskImage::Url(_)));
    }

    #[test]
    fn mask_position_applies_per_layer() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask-image: url(a.png), url(b.png); mask-position: 0% 0%, 10px 20px }",
        );
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask.len(), 2);
        assert_eq!(
            mask[0].position,
            (LengthPct::Percent(0.0), LengthPct::Percent(0.0))
        );
        assert_eq!(mask[1].position, (LengthPct::Px(10.0), LengthPct::Px(20.0)));
    }

    #[test]
    fn mask_size_repeat_cycle_per_layer() {
        // One repeat value cycles across both layers.
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask-image: url(a.png), url(b.png); mask-repeat: no-repeat }",
        );
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask.len(), 2);
        assert_eq!(mask[0].repeat, BgRepeat::NoRepeat);
        assert_eq!(mask[1].repeat, BgRepeat::NoRepeat); // cycled
    }

    #[test]
    fn mask_shorthand_image_and_position() {
        let (doc, t) = style("<div>x</div>", "div { mask: url(a.png) 10px 20px }");
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask.len(), 1);
        assert!(matches!(mask[0].image, MaskImage::Url(_)));
        assert_eq!(mask[0].position, (LengthPct::Px(10.0), LengthPct::Px(20.0)));
    }

    #[test]
    fn mask_shorthand_position_and_size() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask: url(a.png) 10px 20px / 50px 60px }",
        );
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask.len(), 1);
        assert_eq!(mask[0].position, (LengthPct::Px(10.0), LengthPct::Px(20.0)));
        assert_eq!(
            mask[0].size,
            computed::BgSize::Explicit(
                computed::BgSizeAxis::Px(50.0),
                computed::BgSizeAxis::Px(60.0)
            )
        );
    }

    #[test]
    fn mask_shorthand_two_layers_with_repeat() {
        let (doc, t) = style(
            "<div>x</div>",
            "div { mask: linear-gradient(black, white) no-repeat, url(b.png) }",
        );
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask.len(), 2);
        assert!(matches!(mask[0].image, MaskImage::Gradient(_)));
        assert_eq!(mask[0].repeat, BgRepeat::NoRepeat);
        assert!(matches!(mask[1].image, MaskImage::Url(_)));
        assert_eq!(mask[1].repeat, BgRepeat::Repeat); // default
    }

    #[test]
    fn webkit_mask_shorthand_alias() {
        let (doc, t) = style("<div>x</div>", "div { -webkit-mask: url(a.png) }");
        let mask = t.computed(find(&doc, "div")).mask.clone();
        assert_eq!(mask.len(), 1);
        assert!(matches!(mask[0].image, MaskImage::Url(_)));
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

    // --- E33-M3: Shadow DOM style encapsulation + :host / ::slotted ---

    fn orange() -> Rgba {
        Rgba {
            r: 255,
            g: 165,
            b: 0,
            a: 255,
        }
    }

    /// Build a document with one host `<div id=host>` (a light `<p>` is inside as
    /// a slotted child), attach an open shadow root, and populate the shadow tree
    /// via the callback (given the doc + shadow root id). Returns the styled tree.
    fn shadow_doc(
        doc_author_css: &str,
        light: &str,
        build_shadow: impl FnOnce(&mut Document, NodeId),
    ) -> (Document, StyledTree) {
        let mut doc = parse(&format!("<div id=host>{light}</div>"));
        let host = find_id(&doc, "host");
        let sr = doc.attach_shadow(host, starfish_dom::ShadowMode::Open);
        build_shadow(&mut doc, sr);
        let sheet = parse_stylesheet(doc_author_css);
        let tree = style_tree(&doc, &[sheet]);
        (doc, tree)
    }

    /// Append a `<style>` (with the given CSS as a text child) to `parent`.
    fn append_style(doc: &mut Document, parent: NodeId, css: &str) {
        let st = doc.create_element("style");
        let txt = doc.create_text(css);
        doc.append_child(st, txt);
        doc.append_child(parent, st);
    }

    #[test]
    fn shadow_style_is_encapsulated() {
        // Document rule colors <p> blue; the shadow scope colors its own <p> red.
        // Neither leaks across the boundary. The light <p> lives outside the host
        // so it is always part of the (light) render.
        let mut doc = parse("<div id=host></div><p id=light>light</p>");
        let host = find_id(&doc, "host");
        let sr = doc.attach_shadow(host, starfish_dom::ShadowMode::Open);
        append_style(&mut doc, sr, "p { color: red }");
        let p = doc.create_element("p");
        let txt = doc.create_text("shadow");
        doc.append_child(p, txt);
        doc.append_child(sr, p);
        let t = style_tree(&doc, &[parse_stylesheet("p { color: blue }")]);

        let shadow_p = doc
            .children(sr)
            .into_iter()
            .find(|n| doc.tag_name(*n) == Some("p"))
            .unwrap();
        let light_p = find_id(&doc, "light");
        assert_eq!(
            t.computed(shadow_p).color,
            red(),
            "shadow <p> uses shadow rule, NOT the document rule"
        );
        assert_eq!(
            t.computed(light_p).color,
            blue(),
            "light <p> uses document rule, NOT the shadow rule"
        );
    }

    #[test]
    fn host_selector_styles_host() {
        let (doc, t) = shadow_doc("", "x", |doc, sr| {
            append_style(doc, sr, ":host { background-color: green }");
        });
        let host = find_id(&doc, "host");
        assert_eq!(t.computed(host).background_color, green());
    }

    #[test]
    fn host_functional_matches_class() {
        // :host(.on) only applies when the host has class `on`.
        let (doc_on, t_on) = {
            let mut doc = parse("<div id=host class=on>x</div>");
            let host = find_id(&doc, "host");
            let sr = doc.attach_shadow(host, starfish_dom::ShadowMode::Open);
            append_style(&mut doc, sr, ":host(.on) { background-color: green }");
            let tree = style_tree(&doc, &[parse_stylesheet("")]);
            (doc, tree)
        };
        assert_eq!(
            t_on.computed(find_id(&doc_on, "host")).background_color,
            green()
        );

        let (doc_off, t_off) = shadow_doc("", "x", |doc, sr| {
            append_style(doc, sr, ":host(.on) { background-color: green }");
        });
        // Host has no class `on` → :host(.on) does not match → initial bg.
        assert_eq!(
            t_off.computed(find_id(&doc_off, "host")).background_color,
            ComputedStyle::initial().background_color
        );
    }

    #[test]
    fn slotted_styles_distributed_child() {
        // ::slotted(span) colors a slotted light <span>, but NOT a shadow <span>.
        let (doc, t) = shadow_doc("", "<span>light</span>", |doc, sr| {
            append_style(doc, sr, "::slotted(span) { color: #ffa500 }");
            let slot = doc.create_element("slot");
            doc.append_child(sr, slot);
            // A non-slotted shadow span (should NOT be colored by ::slotted).
            let shadow_span = doc.create_element("span");
            doc.append_child(sr, shadow_span);
        });
        let light_span = find(&doc, "span"); // first <span> = light, distributed.
        let sr = doc.shadow_root(find_id(&doc, "host")).unwrap();
        let shadow_span = doc
            .children(sr)
            .into_iter()
            .find(|n| doc.tag_name(*n) == Some("span"))
            .unwrap();
        assert_eq!(
            t.computed(light_span).color,
            orange(),
            "slotted light span colored"
        );
        assert_eq!(
            t.computed(shadow_span).color,
            ComputedStyle::initial().color,
            "non-slotted shadow span not colored by ::slotted"
        );
    }
}
