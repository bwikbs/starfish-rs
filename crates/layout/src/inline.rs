//! Inline layout (§4): greedy word-wrap into `LineBox` children with `TextRun`
//! fragments. text-align Left/Center/Right honored (Justify → Left).

use starfish_style::{LineHeight, StyledTree, TextAlign};

use crate::boxtree::{style_of, BoxKind, BoxStyleRef, LayoutBox};
use crate::dimensions::Rect;
use crate::measure::TextMeasurer;

/// Used line-height in px for a style (§4.3).
fn used_line_height(font_size: f32, lh: LineHeight) -> f32 {
    match lh {
        LineHeight::Px(v) => v,
        LineHeight::Number(n) => n * font_size,
        LineHeight::Normal => 1.2 * font_size,
    }
}

/// One word placed during inline flow, before it is committed to a line.
struct PlacedWord {
    text: String,
    style_ref: BoxStyleRef,
    line_height: f32,
    /// x relative to the inline container's content origin.
    x: f32,
    width: f32,
}

/// A word collected from the flattened inline run, with the metadata needed to
/// measure it and decide whether a collapsed space precedes it.
struct CollectedWord {
    text: String,
    style_ref: BoxStyleRef,
    font_size: f32,
    line_height: LineHeight,
    /// True iff source whitespace was collapsed immediately before this word
    /// within the flattened inline run (false for the very first word).
    space_before: bool,
}

/// Flatten the inline subtree into a flat list of words in document order,
/// carrying a per-word `space_before` flag. Inline boxes are flattened into the
/// flow (their box-model is ignored for M4, §4.2/§7).
///
/// `pending_space` threads collapsed whitespace across run boundaries: a run's
/// leading/trailing space (kept by `collapse_ws`) sets it so the next emitted
/// word records the space, even though that word may live in a different run.
fn collect_words(
    b: &LayoutBox,
    styled: &StyledTree,
    out: &mut Vec<CollectedWord>,
    pending_space: &mut bool,
) {
    for child in &b.children {
        match child.kind {
            BoxKind::TextRun => {
                let style = style_of(styled, child);
                let text = child.text.clone().unwrap_or_default();
                if text.starts_with(' ') {
                    *pending_space = true;
                }
                for word in text.split(' ') {
                    if word.is_empty() {
                        continue;
                    }
                    // First word ever has nothing before it; `pending_space`
                    // starts false so that case is covered.
                    out.push(CollectedWord {
                        text: word.to_string(),
                        style_ref: child.style.clone(),
                        font_size: style.font_size,
                        line_height: style.line_height,
                        space_before: *pending_space,
                    });
                    // Words inside a run are single-space separated, so every
                    // subsequent word in this run is preceded by a space.
                    *pending_space = true;
                }
                // A trailing space carries over to the next run's first word;
                // otherwise the next word abuts directly.
                *pending_space = text.ends_with(' ');
            }
            BoxKind::InlineBox => collect_words(child, styled, out, pending_space),
            _ => {}
        }
    }
}

/// Lay out the inline-level children of `b` into `LineBox` children. Returns the
/// total height of all line boxes (consumed by the block as its content height
/// when `height: auto`).
pub(crate) fn layout_inline(b: &mut LayoutBox, styled: &StyledTree, m: &dyn TextMeasurer) -> f32 {
    let container_style = style_of(styled, b);
    let avail = b.dimensions.content.width;
    let origin = b.dimensions.content;
    let align = container_style.text_align;

    let mut words: Vec<CollectedWord> = Vec::new();
    let mut pending_space = false;
    collect_words(b, styled, &mut words, &mut pending_space);

    // Greedy line breaking. Each line is a Vec<PlacedWord>.
    let mut lines: Vec<Vec<PlacedWord>> = Vec::new();
    let mut cur: Vec<PlacedWord> = Vec::new();
    let mut cursor_x = 0.0f32;

    for cw in words {
        let CollectedWord { text: word, style_ref, font_size, line_height: lh, space_before } = cw;
        let style = match &style_ref {
            BoxStyleRef::Node(id) | BoxStyleRef::Anonymous(id) => styled.get(*id),
        };
        let weight = style.map(|s| s.font_weight).unwrap_or(container_style.font_weight);
        let w = m.measure(&word, font_size, weight);
        let space_w = if space_before { m.measure(" ", font_size, weight) } else { 0.0 };
        let line_h = used_line_height(font_size, lh);

        if cur.is_empty() {
            // line start: place the word (drop any leading space)
            cur.push(PlacedWord { text: word, style_ref, line_height: line_h, x: 0.0, width: w });
            cursor_x = w;
        } else if cursor_x + space_w + w <= avail {
            let x = cursor_x + space_w;
            cur.push(PlacedWord { text: word, style_ref, line_height: line_h, x, width: w });
            cursor_x = x + w;
        } else {
            // break: finish current line, start a new one with this word
            lines.push(std::mem::take(&mut cur));
            cur.push(PlacedWord { text: word, style_ref, line_height: line_h, x: 0.0, width: w });
            cursor_x = w;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }

    // Build LineBox children with absolute geometry.
    let mut line_y = 0.0f32;
    let mut line_boxes: Vec<LayoutBox> = Vec::new();

    for line in lines {
        // `lines` only holds non-empty lines, so the max over word line-heights
        // is always defined.
        let line_height = line.iter().map(|w| w.line_height).fold(0.0f32, f32::max);

        // text-align offset: shift fragments right by slack (right) or slack/2.
        let used_width = line.iter().map(|w| w.x + w.width).fold(0.0f32, f32::max);
        let slack = (avail - used_width).max(0.0);
        let offset = match align {
            TextAlign::Right => slack,
            TextAlign::Center => slack / 2.0,
            TextAlign::Left | TextAlign::Justify => 0.0,
        };

        let mut lb = LayoutBox::new(BoxKind::LineBox, b.style.clone());
        lb.dimensions.content = Rect {
            x: origin.x,
            y: origin.y + line_y,
            width: avail,
            height: line_height,
        };

        for w in line {
            let mut frag = LayoutBox::new(BoxKind::TextRun, w.style_ref);
            frag.text = Some(w.text);
            frag.dimensions.content = Rect {
                x: origin.x + offset + w.x,
                y: origin.y + line_y,
                width: w.width,
                height: w.line_height,
            };
            lb.children.push(frag);
        }

        line_boxes.push(lb);
        line_y += line_height;
    }

    b.children = line_boxes;
    line_y
}
