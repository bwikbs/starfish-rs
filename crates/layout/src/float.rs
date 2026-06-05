//! Float context (§3.1): per-BFC tracking of placed floats in absolute page
//! coordinates, plus the band queries inline + block layout use to flow around
//! them.

use crate::dimensions::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FloatSide {
    Left,
    Right,
}

/// Which float side(s) a `clear` (or a float drop-down) considers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClearSides {
    None,
    Left,
    Right,
    Both,
}

#[derive(Debug, Clone, Copy)]
struct PlacedFloat {
    side: FloatSide,
    /// The float's margin-box rect in absolute page space.
    rect: Rect,
}

#[derive(Debug, Default)]
pub(crate) struct FloatContext {
    floats: Vec<PlacedFloat>,
}

/// True iff float rect `r` overlaps the vertical band `[y, y+height)`. A
/// zero-height band uses a single-point overlap test (`r.y <= y < r.bottom`).
fn overlaps(r: &Rect, y: f32, height: f32) -> bool {
    let r_bottom = r.y + r.height;
    if height <= 0.0 {
        r.y <= y && r_bottom > y
    } else {
        r.y < y + height && r_bottom > y
    }
}

impl FloatContext {
    /// Left inset at band `[y, y+height)`: how far in from `cb_left` content must
    /// start because of left floats overlapping that band.
    pub(crate) fn left_offset(&self, y: f32, height: f32, cb_left: f32) -> f32 {
        let mut inset = 0.0f32;
        for f in &self.floats {
            if f.side == FloatSide::Left && overlaps(&f.rect, y, height) {
                inset = inset.max((f.rect.x + f.rect.width) - cb_left);
            }
        }
        inset.max(0.0)
    }

    /// Right inset at band `[y, y+height)`: how far in from `cb_right` content
    /// must stop because of right floats overlapping that band.
    pub(crate) fn right_offset(&self, y: f32, height: f32, cb_right: f32) -> f32 {
        let mut inset = 0.0f32;
        for f in &self.floats {
            if f.side == FloatSide::Right && overlaps(&f.rect, y, height) {
                inset = inset.max(cb_right - f.rect.x);
            }
        }
        inset.max(0.0)
    }

    /// Lowest bottom edge among floats of the given side(s), or `y_floor` if
    /// none — used for `clear` and float drop-down.
    pub(crate) fn clearance_y(&self, sides: ClearSides, y_floor: f32) -> f32 {
        let mut floor = y_floor;
        for f in &self.floats {
            let considered = match sides {
                ClearSides::None => false,
                ClearSides::Left => f.side == FloatSide::Left,
                ClearSides::Right => f.side == FloatSide::Right,
                ClearSides::Both => true,
            };
            if considered {
                floor = floor.max(f.rect.y + f.rect.height);
            }
        }
        floor
    }

    /// Record a placed float (margin-box rect in absolute coords).
    pub(crate) fn add(&mut self, side: FloatSide, rect: Rect) {
        self.floats.push(PlacedFloat { side, rect });
    }
}
