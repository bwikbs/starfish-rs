//! The flat display list (M5 §3): a pre-order walk of the box tree turning each
//! box into background/border fill-rects and each text run into a glyph run, in
//! correct paint order (parent before child; bg → border → text).

use starfish_layout::{BoxKind, LayoutBox, Rect};
use starfish_style::{
    BorderStyle, ComputedStyle, Float, FontWeight, Position, Rgba, StyledTree, TextDecorationLine,
};

use crate::font::FontDb;

/// A device-space (page-space) paint command. Coordinates are f32 page pixels;
/// the rasterizer rounds. Colors are straight (non-premultiplied) `Rgba`.
#[derive(Debug, Clone, PartialEq)]
pub enum PaintCmd {
    /// A filled rectangle (a background or one border edge).
    FillRect { rect: Rect, color: Rgba },
    /// A run of text. `origin` is the content rect's top-left; the baseline is
    /// `origin.1 + ascent`.
    GlyphRun {
        origin: (f32, f32),
        text: String,
        font_size: f32,
        weight: FontWeight,
        color: Rgba,
        ascent: f32,
    },
}

/// Paint role of a box, deciding which pass paints its subtree (§5). Order of
/// precedence: positioned > float > in-flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    InFlow,
    Float,
    Positioned,
}

fn role(b: &LayoutBox, styled: &StyledTree) -> Role {
    // Only genuine element boxes carry float/position; line/anonymous/text/marker
    // boxes borrow the container's style ref, so never reclassify them.
    if !matches!(
        b.kind(),
        BoxKind::BlockContainer | BoxKind::InlineBlock | BoxKind::InlineBox
    ) {
        return Role::InFlow;
    }
    let Some(s) = b.style(styled) else { return Role::InFlow };
    if s.position != Position::Static {
        Role::Positioned
    } else if s.float != Float::None {
        Role::Float
    } else {
        Role::InFlow
    }
}

/// Walk the laid-out box tree and produce the ordered display list, in three
/// passes (§5): in-flow content, then floats, then positioned boxes — each in
/// tree order, so floats/positioned paint on top. Floats/positioned subtrees
/// recursively re-run the three-pass ordering, so nesting layers correctly.
pub fn build_display_list(root: &LayoutBox, styled: &StyledTree, fonts: &FontDb) -> Vec<PaintCmd> {
    let mut out = Vec::new();
    paint_subtree(root, styled, fonts, &mut out);
    out
}

/// Paint one subtree rooted at `b` (whose own role is fixed by its caller): emit
/// `b` itself + its in-flow descendants, then its float descendants (tree
/// order), then its positioned descendants (tree order). Each deferred subtree
/// recurses through `paint_subtree`, so nested out-of-flow content layers right.
fn paint_subtree(b: &LayoutBox, styled: &StyledTree, fonts: &FontDb, out: &mut Vec<PaintCmd>) {
    let mut floats: Vec<&LayoutBox> = Vec::new();
    let mut positioned: Vec<&LayoutBox> = Vec::new();

    emit_self(b, styled, fonts, out);
    for child in b.children() {
        collect_inflow(child, styled, fonts, out, &mut floats, &mut positioned);
    }
    for f in floats {
        paint_subtree(f, styled, fonts, out);
    }
    for p in positioned {
        paint_subtree(p, styled, fonts, out);
    }
}

/// Pre-order over the in-flow content of a subtree, emitting in-flow boxes and
/// deferring out-of-flow subtree roots into the float / positioned buckets.
fn collect_inflow<'a>(
    b: &'a LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    out: &mut Vec<PaintCmd>,
    floats: &mut Vec<&'a LayoutBox>,
    positioned: &mut Vec<&'a LayoutBox>,
) {
    match role(b, styled) {
        Role::Float => floats.push(b),
        Role::Positioned => positioned.push(b),
        Role::InFlow => {
            emit_self(b, styled, fonts, out);
            for child in b.children() {
                collect_inflow(child, styled, fonts, out, floats, positioned);
            }
        }
    }
}

/// Emit this box's own paint commands (bg/border, or text for text/marker runs).
fn emit_self(b: &LayoutBox, styled: &StyledTree, fonts: &FontDb, out: &mut Vec<PaintCmd>) {
    match b.kind() {
        BoxKind::TextRun | BoxKind::Marker => emit_text(b, styled, fonts, out),
        _ => {
            emit_background(b, styled, out);
            emit_borders(b, styled, out);
        }
    }
}

fn emit_background(b: &LayoutBox, styled: &StyledTree, out: &mut Vec<PaintCmd>) {
    let Some(style) = b.style(styled) else { return };
    let bg = style.background_color;
    if bg.a == 0 {
        return;
    }
    out.push(PaintCmd::FillRect {
        rect: b.dimensions().border_box(),
        color: bg,
    });
}

fn emit_borders(b: &LayoutBox, styled: &StyledTree, out: &mut Vec<PaintCmd>) {
    let Some(style) = b.style(styled) else { return };
    if style.border_style != BorderStyle::Solid {
        return;
    }
    let bc = style.border_color;
    if bc.a == 0 {
        return;
    }
    let d = b.dimensions();
    let bb = d.border_box();

    if d.border.top > 0.0 {
        out.push(PaintCmd::FillRect {
            rect: Rect { x: bb.x, y: bb.y, width: bb.width, height: d.border.top },
            color: bc,
        });
    }
    if d.border.bottom > 0.0 {
        out.push(PaintCmd::FillRect {
            rect: Rect {
                x: bb.x,
                y: bb.y + bb.height - d.border.bottom,
                width: bb.width,
                height: d.border.bottom,
            },
            color: bc,
        });
    }
    if d.border.left > 0.0 {
        out.push(PaintCmd::FillRect {
            rect: Rect {
                x: bb.x,
                y: bb.y + d.border.top,
                width: d.border.left,
                height: bb.height - d.border.top - d.border.bottom,
            },
            color: bc,
        });
    }
    if d.border.right > 0.0 {
        out.push(PaintCmd::FillRect {
            rect: Rect {
                x: bb.x + bb.width - d.border.right,
                y: bb.y + d.border.top,
                width: d.border.right,
                height: bb.height - d.border.top - d.border.bottom,
            },
            color: bc,
        });
    }
}

fn emit_text(b: &LayoutBox, styled: &StyledTree, fonts: &FontDb, out: &mut Vec<PaintCmd>) {
    let initial = ComputedStyle::initial();
    let style = b.style(styled).unwrap_or(&initial);
    let text = b.text().unwrap_or("");
    if text.is_empty() {
        return;
    }
    let c = b.dimensions().content;
    let lm = fonts.line_metrics(style.font_size, style.font_weight);
    out.push(PaintCmd::GlyphRun {
        origin: (c.x, c.y),
        text: text.to_string(),
        font_size: style.font_size,
        weight: style.font_weight,
        color: style.color,
        ascent: lm.ascent,
    });

    // text-decoration lines — only for real text runs, never markers (§4.1).
    if b.kind() != BoxKind::TextRun {
        return;
    }
    let deco = style.text_decoration_line;
    if deco.is_none() {
        return;
    }
    let thickness = (style.font_size / 16.0).max(1.0);
    let color = style.color; // decoration color = text color
    let baseline = c.y + lm.ascent;
    let mut line = |y: f32| {
        out.push(PaintCmd::FillRect {
            rect: Rect { x: c.x, y, width: c.width, height: thickness },
            color,
        });
    };
    if deco.contains(TextDecorationLine::UNDERLINE) {
        line(baseline + 1.0); // just below baseline
    }
    if deco.contains(TextDecorationLine::LINE_THROUGH) {
        line(baseline - lm.ascent * 0.3); // ~middle / x-height
    }
    if deco.contains(TextDecorationLine::OVERLINE) {
        line(c.y); // top of the content box
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_css::parse_stylesheet;
    use starfish_html::parse;
    use starfish_layout::layout;
    use starfish_style::style_tree;

    use crate::font::FontMeasurer;

    fn list(html: &str, css: &str) -> Vec<PaintCmd> {
        let doc = parse(html);
        let sheet = parse_stylesheet(css);
        let styled = style_tree(&doc, &[sheet]);
        let fonts = FontDb::load().unwrap();
        let root = layout(&doc, &styled, 800.0, &FontMeasurer(&fonts));
        build_display_list(&root, &styled, &fonts)
    }

    #[test]
    fn background_before_border_before_text() {
        let cmds = list(
            "<html><body><div id='d'><p>hi</p></div></body></html>",
            "body{margin:0} #d{background:#ff0000;border:2px solid #0000ff}",
        );
        // first fill is the div background (red), then border fills (blue),
        // then the glyph run.
        let first_bg = cmds.iter().position(|c| {
            matches!(c, PaintCmd::FillRect { color, .. } if color.r == 255 && color.b == 0)
        });
        let first_border = cmds.iter().position(|c| {
            matches!(c, PaintCmd::FillRect { color, .. } if color.b == 255 && color.r == 0)
        });
        let first_glyph = cmds.iter().position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
        let (bg, border, glyph) = (
            first_bg.expect("bg"),
            first_border.expect("border"),
            first_glyph.expect("glyph"),
        );
        assert!(bg < border, "bg {bg} before border {border}");
        assert!(border < glyph, "border {border} before glyph {glyph}");
    }

    #[test]
    fn div_with_bg_emits_fillrect_at_its_rect() {
        let cmds = list(
            "<html><body><div id='d'>x</div></body></html>",
            "body{margin:0} #d{width:100px;height:50px;background:#00ff00}",
        );
        let found = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, rect }
                if color.g == 255 && color.r == 0 && rect.width == 100.0
        ));
        assert!(found, "expected a green 100px-wide fill rect: {cmds:?}");
    }

    #[test]
    fn transparent_no_border_emits_no_fillrect() {
        let cmds = list("<html><body><p>hi</p></body></html>", "body{margin:0}");
        assert!(
            !cmds.iter().any(|c| matches!(c, PaintCmd::FillRect { .. })),
            "no fills expected for a plain paragraph: {cmds:?}"
        );
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { .. })));
    }

    #[test]
    fn text_run_carries_parent_color_and_size() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{color:#0000ff;font-size:20px}",
        );
        let glyph = cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { color, font_size, text, .. } => Some((*color, *font_size, text.clone())),
            _ => None,
        });
        let (color, fs, text) = glyph.expect("glyph run");
        assert_eq!(color, Rgba { r: 0, g: 0, b: 255, a: 255 });
        assert_eq!(fs, 20.0);
        assert_eq!(text, "hi");
    }

    // --- E2-M1: text-decoration, list markers, inline-block ---

    /// The glyph run + its content rect for the first text run matching `t`.
    fn glyph_with_origin(cmds: &[PaintCmd], t: &str) -> (f32, f32, f32) {
        cmds.iter().find_map(|c| match c {
            PaintCmd::GlyphRun { origin, text, .. } if text == t => Some((origin.0, origin.1, 0.0)),
            _ => None,
        }).unwrap_or_else(|| panic!("no glyph run {t:?}"))
    }

    #[test]
    fn underline_emits_fillrect_below_baseline() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;color:#000000;font-size:20px;text-decoration:underline}",
        );
        // exactly one fill rect (the underline) at baseline+1.
        let fills: Vec<&PaintCmd> = cmds.iter().filter(|c| matches!(c, PaintCmd::FillRect { .. })).collect();
        assert_eq!(fills.len(), 1, "expected one underline rect: {cmds:?}");
        // locate the glyph run to recover its content x/y and width.
        let (gx, gy, _) = glyph_with_origin(&cmds, "hi");
        let (rect, color) = match fills[0] {
            PaintCmd::FillRect { rect, color } => (*rect, *color),
            _ => unreachable!(),
        };
        assert_eq!(rect.x, gx);
        assert!(rect.width > 0.0);
        assert_eq!(rect.height, (20.0f32 / 16.0).max(1.0));
        assert_eq!(color, Rgba { r: 0, g: 0, b: 0, a: 255 });
        // y ≈ content.y + ascent + 1; assert it's below the glyph origin.
        assert!(rect.y > gy);
    }

    #[test]
    fn combined_decoration_emits_three_rects() {
        let cmds = list(
            "<html><body><p>hi</p></body></html>",
            "body{margin:0} p{margin:0;font-size:20px;\
             text-decoration-line:underline overline line-through}",
        );
        let fills = cmds.iter().filter(|c| matches!(c, PaintCmd::FillRect { .. })).count();
        assert_eq!(fills, 3, "underline+overline+line-through: {cmds:?}");
    }

    #[test]
    fn marker_emits_bullet_glyph() {
        let cmds = list("<html><body><ul><li>a</li></ul></body></html>", "body{margin:0}");
        assert!(
            cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "\u{2022}")),
            "expected a bullet glyph run: {cmds:?}"
        );
    }

    #[test]
    fn decimal_marker_emits_number_glyph() {
        let cmds = list("<html><body><ol><li>x</li></ol></body></html>", "body{margin:0}");
        assert!(
            cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "1.")),
            "expected a '1.' glyph run: {cmds:?}"
        );
    }

    #[test]
    fn inline_block_paints_its_background() {
        let cmds = list(
            "<html><body><div><span class='ib'>x</span></div></body></html>",
            "body{margin:0} div{margin:0} \
             .ib{display:inline-block;width:50px;height:20px;background:#00ff00}",
        );
        let found = cmds.iter().any(|c| matches!(
            c,
            PaintCmd::FillRect { color, rect }
                if color.g == 255 && color.r == 0 && rect.width == 50.0
        ));
        assert!(found, "expected a green 50px-wide inline-block bg: {cmds:?}");
    }

    // --- E2-M2: float / positioned paint ordering ---

    /// Index of the first FillRect whose color matches a predicate.
    fn first_fill(cmds: &[PaintCmd], pred: impl Fn(&Rgba) -> bool) -> Option<usize> {
        cmds.iter()
            .position(|c| matches!(c, PaintCmd::FillRect { color, .. } if pred(color)))
    }

    #[test]
    fn paint_order_inflow_then_float_then_positioned() {
        // An in-flow div (red bg), a left float (green bg), and an absolute div
        // (blue bg): float bg paints after in-flow bg; absolute after float.
        let cmds = list(
            "<html><body><div id='wrap'>\
             <div id='n'></div>\
             <div id='f'></div>\
             <div id='a'></div>\
             </div></body></html>",
            "body{margin:0} #wrap{position:relative} \
             #n{background:#ff0000;height:20px} \
             #f{float:left;width:40px;height:20px;background:#00ff00} \
             #a{position:absolute;top:0;left:0;width:30px;height:20px;background:#0000ff}",
        );
        let red = first_fill(&cmds, |c| c.r == 255 && c.g == 0 && c.b == 0).expect("in-flow red bg");
        let green = first_fill(&cmds, |c| c.g == 255 && c.r == 0 && c.b == 0).expect("float green bg");
        let blue = first_fill(&cmds, |c| c.b == 255 && c.r == 0 && c.g == 0).expect("abs blue bg");
        assert!(red < green, "in-flow {red} before float {green}");
        assert!(green < blue, "float {green} before positioned {blue}");
    }

    #[test]
    fn inflow_only_display_list_unchanged() {
        // The existing in-flow-only corpus must produce an identical display list
        // under the new three-pass build_display_list (passes 2/3 empty).
        let cmds = list(
            "<html><body><div id='d'><p>hi</p></div></body></html>",
            "body{margin:0} #d{background:#ff0000;border:2px solid #0000ff}",
        );
        // Same shape/order as background_before_border_before_text expects.
        let bg = cmds.iter().position(|c| {
            matches!(c, PaintCmd::FillRect { color, .. } if color.r == 255 && color.b == 0)
        });
        let border = cmds.iter().position(|c| {
            matches!(c, PaintCmd::FillRect { color, .. } if color.b == 255 && color.r == 0)
        });
        let glyph = cmds.iter().position(|c| matches!(c, PaintCmd::GlyphRun { .. }));
        let (bg, border, glyph) = (bg.expect("bg"), border.expect("border"), glyph.expect("glyph"));
        assert!(bg < border && border < glyph);
        // No float/positioned content → display list is exactly the in-flow walk:
        // div bg + 4 border edges + glyph = 6 commands.
        assert_eq!(cmds.len(), 6, "unexpected extra commands: {cmds:?}");
    }

    #[test]
    fn marker_is_not_decorated() {
        // <ul> with underline; the bullet glyph must have no decoration rect.
        let cmds = list(
            "<html><body><ul><li>a</li></ul></body></html>",
            "body{margin:0} ul{text-decoration:underline} li{text-decoration:underline}",
        );
        // the bullet glyph exists...
        assert!(cmds.iter().any(|c| matches!(c, PaintCmd::GlyphRun { text, .. } if text == "\u{2022}")));
        // ...but only the "a" TextRun produces an underline rect, not the marker.
        // There's exactly one decoration FillRect (for "a").
        let fills = cmds.iter().filter(|c| matches!(c, PaintCmd::FillRect { .. })).count();
        assert_eq!(fills, 1, "only the text run is decorated, not the marker: {cmds:?}");
    }
}
