//! The flat display list (M5 §3): a pre-order walk of the box tree turning each
//! box into background/border fill-rects and each text run into a glyph run, in
//! correct paint order (parent before child; bg → border → text).

use starfish_layout::{BoxKind, LayoutBox, Rect};
use starfish_style::{BorderStyle, ComputedStyle, FontWeight, Rgba, StyledTree};

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

/// Walk the laid-out box tree and produce the ordered display list.
pub fn build_display_list(root: &LayoutBox, styled: &StyledTree, fonts: &FontDb) -> Vec<PaintCmd> {
    let mut out = Vec::new();
    paint_box(root, styled, fonts, &mut out);
    out
}

fn paint_box(b: &LayoutBox, styled: &StyledTree, fonts: &FontDb, out: &mut Vec<PaintCmd>) {
    match b.kind() {
        BoxKind::TextRun => emit_text(b, styled, fonts, out),
        _ => {
            emit_background(b, styled, out);
            emit_borders(b, styled, out);
        }
    }
    for child in b.children() {
        paint_box(child, styled, fonts, out);
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
}
