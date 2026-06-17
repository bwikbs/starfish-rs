//! Float context (§3.1): per-BFC tracking of placed floats in absolute page
//! coordinates, plus the band queries inline + block layout use to flow around
//! them.

use crate::dimensions::Rect;
use starfish_style::{ClipRadius, ClipShape, LengthPct};

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

#[derive(Debug, Clone)]
struct PlacedFloat {
    side: FloatSide,
    /// The float's margin-box rect in absolute page space.
    rect: Rect,
    /// `shape-outside` basic shape (E65-M1), resolved against `rect`. Only
    /// `Circle` narrows the exclusion band this milestone; other shapes / `None`
    /// fall back to the rect edge.
    shape: Option<ClipShape>,
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

/// Resolve a `<length-percentage>` against `basis` (matches the paint crate's
/// `resolve_lp`).
fn resolve_lp(v: LengthPct, basis: f32) -> f32 {
    match v {
        LengthPct::Px(p) => p,
        LengthPct::Percent(p) => p / 100.0 * basis,
    }
}

/// Resolve a circle radius in a `w`×`h` reference box with center `(cx, cy)`
/// (matches the paint crate's `resolve_circle_radius`; percentage radius uses
/// the CSS Shapes formula `p · sqrt(w²+h²)/√2`).
fn resolve_circle_radius(r: ClipRadius, w: f32, h: f32, cx: f32, cy: f32) -> f32 {
    match r {
        ClipRadius::Length(LengthPct::Px(v)) => v,
        ClipRadius::Length(LengthPct::Percent(p)) => {
            p / 100.0 * (w * w + h * h).sqrt() / std::f32::consts::SQRT_2
        }
        ClipRadius::ClosestSide => cx.min(w - cx).min(cy).min(h - cy).max(0.0),
        ClipRadius::FarthestSide => cx.max(w - cx).max(cy).max(h - cy),
    }
}

/// The circle's horizontal extent at band `[y, y+height)`, resolved in the
/// float's margin box `rect`. Returns `Some((left_edge, right_edge))` in
/// absolute coords when the band overlaps the circle's vertical span, else
/// `None` (the circle excludes nothing for that band).
fn circle_extent_at(rect: &Rect, r: ClipRadius, cx: LengthPct, cy: LengthPct, y: f32, height: f32) -> Option<(f32, f32)> {
    let clx = resolve_lp(cx, rect.width);
    let cly = resolve_lp(cy, rect.height);
    let rad = resolve_circle_radius(r, rect.width, rect.height, clx, cly);
    let cx_abs = rect.x + clx;
    let cy_abs = rect.y + cly;
    let (top, bottom) = (cy_abs - rad, cy_abs + rad);
    let band_bottom = if height <= 0.0 { y } else { y + height };
    // No vertical overlap with the circle's span → no exclusion.
    if band_bottom < top || y > bottom {
        return None;
    }
    // Evaluate the half-width at the band y nearest the circle center (the
    // widest extent over the band = most conservative exclusion).
    let y_eval = cy_abs.clamp(y, band_bottom);
    let dy = y_eval - cy_abs;
    let dx = (rad * rad - dy * dy).max(0.0).sqrt();
    Some((cx_abs - dx, cx_abs + dx))
}

/// The float's effective right edge (its inner edge for a left float) at band
/// `[y, y+height)`: the shape edge for a `Circle`, else the rect edge.
/// Returns `None` when the float excludes nothing for that band.
fn left_edge_at(f: &PlacedFloat, y: f32, height: f32) -> Option<f32> {
    match &f.shape {
        Some(ClipShape::Circle { r, cx, cy }) => {
            circle_extent_at(&f.rect, *r, *cx, *cy, y, height).map(|(_, right)| right)
        }
        _ => Some(f.rect.x + f.rect.width),
    }
}

/// The float's effective left edge (its inner edge for a right float) at band
/// `[y, y+height)`: the shape edge for a `Circle`, else the rect edge.
fn right_edge_at(f: &PlacedFloat, y: f32, height: f32) -> Option<f32> {
    match &f.shape {
        Some(ClipShape::Circle { r, cx, cy }) => {
            circle_extent_at(&f.rect, *r, *cx, *cy, y, height).map(|(left, _)| left)
        }
        _ => Some(f.rect.x),
    }
}

impl FloatContext {
    /// Left inset at band `[y, y+height)`: how far in from `cb_left` content must
    /// start because of left floats overlapping that band.
    pub(crate) fn left_offset(&self, y: f32, height: f32, cb_left: f32) -> f32 {
        let mut inset = 0.0f32;
        for f in &self.floats {
            if f.side == FloatSide::Left && overlaps(&f.rect, y, height) {
                if let Some(edge) = left_edge_at(f, y, height) {
                    inset = inset.max(edge - cb_left);
                }
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
                if let Some(edge) = right_edge_at(f, y, height) {
                    inset = inset.max(cb_right - edge);
                }
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

    /// Record a placed float (margin-box rect in absolute coords) with an
    /// optional `shape-outside` shape resolved against that rect.
    pub(crate) fn add(&mut self, side: FloatSide, rect: Rect, shape: Option<ClipShape>) {
        self.floats.push(PlacedFloat { side, rect, shape });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect { x, y, width: w, height: h }
    }

    /// A circle filling the float's 100×100 margin box (center 50,50; r=50).
    fn circle_full() -> ClipShape {
        ClipShape::Circle {
            r: ClipRadius::ClosestSide,
            cx: LengthPct::Percent(50.0),
            cy: LengthPct::Percent(50.0),
        }
    }

    // (1) A circular left float excludes the full rect width at its vertical
    // center, but less near the top/bottom of the curve.
    #[test]
    fn circle_left_narrows_away_from_center() {
        let mut fc = FloatContext::default();
        fc.add(FloatSide::Left, rect(0.0, 0.0, 100.0, 100.0), Some(circle_full()));
        // Band through the center → full rect edge (right edge = 100).
        let center = fc.left_offset(50.0, 0.0, 0.0);
        assert!((center - 100.0).abs() < 0.01, "center inset = {center}");
        // Band near the top → narrower exclusion (< full width).
        let near_top = fc.left_offset(10.0, 0.0, 0.0);
        assert!(near_top < center, "near_top {near_top} should be < center {center}");
        assert!(near_top > 0.0);
        // Symmetric near the bottom.
        let near_bottom = fc.left_offset(90.0, 0.0, 0.0);
        assert!((near_top - near_bottom).abs() < 0.01);
    }

    // (2) A band inside the rect but entirely outside the circle's vertical
    // span excludes nothing. With cy=10,r=5 the circle spans y∈[5,15]; a band
    // at y=50 (inside the 100-tall rect) is outside it.
    #[test]
    fn circle_band_outside_vertical_span_excludes_nothing() {
        let mut fc = FloatContext::default();
        let shape = ClipShape::Circle {
            r: ClipRadius::Length(LengthPct::Px(5.0)),
            cx: LengthPct::Percent(50.0),
            cy: LengthPct::Px(10.0),
        };
        fc.add(FloatSide::Left, rect(0.0, 0.0, 100.0, 100.0), Some(shape));
        assert_eq!(fc.left_offset(50.0, 0.0, 0.0), 0.0);
        // But a band through the circle still excludes.
        assert!(fc.left_offset(10.0, 0.0, 0.0) > 0.0);
    }

    // (4) A shapeless float is unchanged: rect edges.
    #[test]
    fn shapeless_float_uses_rect_edge() {
        let mut fc = FloatContext::default();
        fc.add(FloatSide::Left, rect(0.0, 0.0, 80.0, 100.0), None);
        assert_eq!(fc.left_offset(0.0, 100.0, 0.0), 80.0);

        let mut rc = FloatContext::default();
        rc.add(FloatSide::Right, rect(200.0, 0.0, 80.0, 100.0), None);
        // cb_right=300, float left edge=200 → exclusion 100.
        assert_eq!(rc.right_offset(0.0, 100.0, 300.0), 100.0);
    }

    // Right-side circle mirrors the left behavior.
    #[test]
    fn circle_right_narrows_away_from_center() {
        let mut fc = FloatContext::default();
        // Float at x∈[200,300], cb_right=300, circle center (250,50) r=50.
        fc.add(FloatSide::Right, rect(200.0, 0.0, 100.0, 100.0), Some(circle_full()));
        let center = fc.right_offset(50.0, 0.0, 300.0);
        assert!((center - 100.0).abs() < 0.01, "center inset = {center}");
        let near_top = fc.right_offset(10.0, 0.0, 300.0);
        assert!(near_top < center && near_top > 0.0);
    }
}
