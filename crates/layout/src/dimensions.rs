//! Geometry primitives: `Rect`, `EdgeSizes`, `Dimensions` and the
//! padding/border/margin box accessors (§1.1 of the M4 design note).

/// An axis-aligned rectangle in absolute page space.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Rect {
    /// Grow this rect outward by the given edge sizes.
    pub fn expanded_by(&self, edge: &EdgeSizes) -> Rect {
        Rect {
            x: self.x - edge.left,
            y: self.y - edge.top,
            width: self.width + edge.left + edge.right,
            height: self.height + edge.top + edge.bottom,
        }
    }
}

/// Per-side sizes (top/right/bottom/left) for margin, border, padding.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EdgeSizes {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

/// Geometry of one box. `content` is the absolute (page-space) content rect;
/// the surrounding edges grow outward from it.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Dimensions {
    pub content: Rect,
    pub padding: EdgeSizes,
    pub border: EdgeSizes,
    pub margin: EdgeSizes,
}

impl Dimensions {
    /// the content rect (the `content-box` geometry box).
    pub fn content_box(&self) -> Rect {
        self.content
    }

    /// content + padding (the background-painting box).
    pub fn padding_box(&self) -> Rect {
        self.content.expanded_by(&self.padding)
    }

    /// padding box + border (the border-painting box).
    pub fn border_box(&self) -> Rect {
        self.padding_box().expanded_by(&self.border)
    }

    /// border box + margin (the space the box occupies among siblings).
    pub fn margin_box(&self) -> Rect {
        self.border_box().expanded_by(&self.margin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn box_accessors_expand_outward() {
        let d = Dimensions {
            content: Rect {
                x: 10.0,
                y: 20.0,
                width: 100.0,
                height: 50.0,
            },
            padding: EdgeSizes {
                top: 1.0,
                right: 2.0,
                bottom: 3.0,
                left: 4.0,
            },
            border: EdgeSizes {
                top: 5.0,
                right: 5.0,
                bottom: 5.0,
                left: 5.0,
            },
            margin: EdgeSizes {
                top: 6.0,
                right: 7.0,
                bottom: 8.0,
                left: 9.0,
            },
        };

        let p = d.padding_box();
        assert_eq!(
            p,
            Rect {
                x: 6.0,
                y: 19.0,
                width: 106.0,
                height: 54.0
            }
        );

        let b = d.border_box();
        assert_eq!(
            b,
            Rect {
                x: 1.0,
                y: 14.0,
                width: 116.0,
                height: 64.0
            }
        );

        let m = d.margin_box();
        assert_eq!(
            m,
            Rect {
                x: -8.0,
                y: 8.0,
                width: 132.0,
                height: 78.0
            }
        );
    }
}
