//! Animatable-value interpolation (E17-M1). Simple per-type lerps used by the
//! `apply_animations` pass to resolve a static frame at a given time.

use crate::computed::{Length, LengthPct};
use starfish_css::Rgba;

/// Linear interpolation of two `f32`s at fraction `t` (`t=0`→`a`, `t=1`→`b`).
pub(crate) fn lerp_f32(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Interpolate a `u8` channel, rounding to nearest.
pub(crate) fn lerp_u8(a: u8, b: u8, t: f32) -> u8 {
    (lerp_f32(a as f32, b as f32, t)).round().clamp(0.0, 255.0) as u8
}

/// Per-channel RGBA interpolation (incl. alpha).
pub(crate) fn lerp_rgba(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba {
        r: lerp_u8(a.r, b.r, t),
        g: lerp_u8(a.g, b.g, t),
        b: lerp_u8(a.b, b.b, t),
        a: lerp_u8(a.a, b.a, t),
    }
}

/// Interpolate two [`Length`]s. `Px`↔`Px` and `Percent`↔`Percent` lerp; any
/// mixed / `Auto` / `Calc` pair falls back to discrete (`b` if `t>=0.5`, else `a`).
pub(crate) fn lerp_length(a: Length, b: Length, t: f32) -> Length {
    match (a, b) {
        (Length::Px(x), Length::Px(y)) => Length::Px(lerp_f32(x, y, t)),
        (Length::Percent(x), Length::Percent(y)) => Length::Percent(lerp_f32(x, y, t)),
        _ => {
            if t >= 0.5 {
                b
            } else {
                a
            }
        }
    }
}

/// Interpolate two [`LengthPct`]s (same rules as [`lerp_length`]). Provided for
/// the transform/origin interpolation that lands in E17-M2; currently only
/// exercised by tests.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn lerp_lengthpct(a: LengthPct, b: LengthPct, t: f32) -> LengthPct {
    match (a, b) {
        (LengthPct::Px(x), LengthPct::Px(y)) => LengthPct::Px(lerp_f32(x, y, t)),
        (LengthPct::Percent(x), LengthPct::Percent(y)) => LengthPct::Percent(lerp_f32(x, y, t)),
        _ => {
            if t >= 0.5 {
                b
            } else {
                a
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lerp_rgba_midpoint() {
        let a = Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let b = Rgba {
            r: 255,
            g: 100,
            b: 50,
            a: 255,
        };
        let m = lerp_rgba(a, b, 0.5);
        assert_eq!((m.r, m.g, m.b, m.a), (128, 50, 25, 255));
    }

    #[test]
    fn lerp_length_px() {
        assert_eq!(
            lerp_length(Length::Px(10.0), Length::Px(20.0), 0.5),
            Length::Px(15.0)
        );
        // mixed → discrete.
        assert_eq!(
            lerp_length(Length::Px(10.0), Length::Auto, 0.6),
            Length::Auto
        );
        assert_eq!(
            lerp_length(Length::Px(10.0), Length::Auto, 0.4),
            Length::Px(10.0)
        );
    }

    #[test]
    fn lerp_f32_endpoints() {
        assert_eq!(lerp_f32(2.0, 8.0, 0.0), 2.0);
        assert_eq!(lerp_f32(2.0, 8.0, 1.0), 8.0);
        assert_eq!(lerp_f32(2.0, 8.0, 0.25), 3.5);
    }

    #[test]
    fn lerp_lengthpct_px_and_mixed() {
        assert_eq!(
            lerp_lengthpct(LengthPct::Px(0.0), LengthPct::Px(10.0), 0.5),
            LengthPct::Px(5.0)
        );
        // mixed → discrete.
        assert_eq!(
            lerp_lengthpct(LengthPct::Px(0.0), LengthPct::Percent(100.0), 0.6),
            LengthPct::Percent(100.0)
        );
    }
}
