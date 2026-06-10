//! Animatable-value interpolation (E17-M1). Simple per-type lerps used by the
//! `apply_animations` pass to resolve a static frame at a given time.

use crate::computed::{BoxShadow, Length, LengthPct, TransformFn};
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
pub(crate) fn lerp_length(a: &Length, b: &Length, t: f32) -> Length {
    match (a, b) {
        (Length::Px(x), Length::Px(y)) => Length::Px(lerp_f32(*x, *y, t)),
        (Length::Percent(x), Length::Percent(y)) => Length::Percent(lerp_f32(*x, *y, t)),
        _ => {
            if t >= 0.5 {
                b.clone()
            } else {
                a.clone()
            }
        }
    }
}

/// Interpolate two [`LengthPct`]s (same rules as [`lerp_length`]). Used by
/// [`lerp_transform`]'s componentwise translate path (E17-M2).
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

/// Interpolate `border-radius` corner-wise (E17-M3): each of TL/TR/BR/BL lerps
/// independently as an `f32`.
pub(crate) fn lerp_radius(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        lerp_f32(a[0], b[0], t),
        lerp_f32(a[1], b[1], t),
        lerp_f32(a[2], b[2], t),
        lerp_f32(a[3], b[3], t),
    ]
}

/// Interpolate `box-shadow` (E17-M3, single-shadow subset). `(None, None)` →
/// `None`; otherwise the absent side is padded with a transparent zero-shadow,
/// then offsets/blur/spread lerp as floats (blur clamped to ≥ 0) and the color
/// lerps per-channel.
pub(crate) fn lerp_box_shadow(
    a: Option<BoxShadow>,
    b: Option<BoxShadow>,
    t: f32,
) -> Option<BoxShadow> {
    if a.is_none() && b.is_none() {
        return None;
    }
    let zero = BoxShadow {
        offset_x: 0.0,
        offset_y: 0.0,
        blur: 0.0,
        spread: 0.0,
        color: Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        },
    };
    let a = a.unwrap_or(zero);
    let b = b.unwrap_or(zero);
    Some(BoxShadow {
        offset_x: lerp_f32(a.offset_x, b.offset_x, t),
        offset_y: lerp_f32(a.offset_y, b.offset_y, t),
        blur: lerp_f32(a.blur, b.blur, t).max(0.0),
        spread: lerp_f32(a.spread, b.spread, t),
        color: lerp_rgba(a.color, b.color, t),
    })
}

/// The identity value of the same kind as `f` (E17-M2). Used to pad the shorter
/// transform list so both sides have the same length before interpolating.
fn identity_of(f: &TransformFn) -> TransformFn {
    match f {
        TransformFn::Translate(..) => {
            TransformFn::Translate(LengthPct::Px(0.0), LengthPct::Px(0.0))
        }
        TransformFn::Scale(..) => TransformFn::Scale(1.0, 1.0),
        TransformFn::Rotate(_) => TransformFn::Rotate(0.0),
        TransformFn::Skew(..) => TransformFn::Skew(0.0, 0.0),
        TransformFn::Matrix(_) => TransformFn::Matrix([1.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
    }
}

/// Multiply two 2D affine matrices in `[a,b,c,d,e,f]` (= tiny-skia `from_row`
/// `sx,ky,kx,sy,tx,ty`) layout: `r = x * y` (apply `y` to points first, then
/// `x`). Mirrors `Transform::pre_concat` used by `display.rs::compose_transform`.
fn mat_mul(x: [f32; 6], y: [f32; 6]) -> [f32; 6] {
    // x: sx=x[0] ky=x[1] kx=x[2] sy=x[3] tx=x[4] ty=x[5]
    [
        x[0] * y[0] + x[2] * y[1],
        x[1] * y[0] + x[3] * y[1],
        x[0] * y[2] + x[2] * y[3],
        x[1] * y[2] + x[3] * y[3],
        x[0] * y[4] + x[2] * y[5] + x[4],
        x[1] * y[4] + x[3] * y[5] + x[5],
    ]
}

/// One [`TransformFn`] → a `[a,b,c,d,e,f]` 2D affine, mirroring
/// `display.rs::fn_matrix` (from_row order `sx,ky,kx,sy,tx,ty`). Origin-free
/// (interpolation is origin-independent) and `Percent` translates fold to `0.0`
/// — style has no box dimensions (documented MVP limitation).
fn fn_affine(f: &TransformFn) -> [f32; 6] {
    let lp = |v: LengthPct| match v {
        LengthPct::Px(p) => p,
        LengthPct::Percent(_) => 0.0,
    };
    match *f {
        TransformFn::Translate(x, y) => [1.0, 0.0, 0.0, 1.0, lp(x), lp(y)],
        TransformFn::Scale(sx, sy) => [sx, 0.0, 0.0, sy, 0.0, 0.0],
        TransformFn::Rotate(t) => [t.cos(), t.sin(), -t.sin(), t.cos(), 0.0, 0.0],
        TransformFn::Skew(ax, ay) => [1.0, ay.tan(), ax.tan(), 1.0, 0.0, 0.0],
        TransformFn::Matrix(m) => m,
    }
}

/// Collapse a transform list to a single `[a,b,c,d,e,f]` affine by left-to-right
/// composition (`f1·f2·…·fn`), mirroring `display.rs::compose_transform` minus
/// the origin terms.
fn collapse(list: &[TransformFn]) -> [f32; 6] {
    let mut acc = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];
    for f in list {
        acc = mat_mul(acc, fn_affine(f));
    }
    acc
}

/// Interpolate two `transform` function lists at fraction `t` (E17-M2). If, after
/// identity-padding the shorter list, every index holds the SAME function kind,
/// interpolate componentwise; otherwise collapse each side to one affine matrix
/// and lerp the six floats (the CSS "matrix fallback").
pub(crate) fn lerp_transform(a: &[TransformFn], b: &[TransformFn], t: f32) -> Vec<TransformFn> {
    let n = a.len().max(b.len());
    if n == 0 {
        return Vec::new();
    }
    // Identity-pad the shorter list with the corresponding function's identity.
    let pad = |list: &[TransformFn], other: &[TransformFn]| -> Vec<TransformFn> {
        let mut v: Vec<TransformFn> = list.to_vec();
        for f in other.iter().skip(list.len()) {
            v.push(identity_of(f));
        }
        v
    };
    let av = pad(a, b);
    let bv = pad(b, a);

    let same_kinds = av
        .iter()
        .zip(&bv)
        .all(|(x, y)| std::mem::discriminant(x) == std::mem::discriminant(y));

    if same_kinds {
        av.iter()
            .zip(&bv)
            .map(|(x, y)| match (*x, *y) {
                (TransformFn::Translate(ax, ay), TransformFn::Translate(bx, by)) => {
                    TransformFn::Translate(lerp_lengthpct(ax, bx, t), lerp_lengthpct(ay, by, t))
                }
                (TransformFn::Scale(asx, asy), TransformFn::Scale(bsx, bsy)) => {
                    TransformFn::Scale(lerp_f32(asx, bsx, t), lerp_f32(asy, bsy, t))
                }
                (TransformFn::Rotate(ar), TransformFn::Rotate(br)) => {
                    TransformFn::Rotate(lerp_f32(ar, br, t))
                }
                (TransformFn::Skew(aax, aay), TransformFn::Skew(bax, bay)) => {
                    TransformFn::Skew(lerp_f32(aax, bax, t), lerp_f32(aay, bay, t))
                }
                (TransformFn::Matrix(am), TransformFn::Matrix(bm)) => {
                    let mut m = [0.0; 6];
                    for i in 0..6 {
                        m[i] = lerp_f32(am[i], bm[i], t);
                    }
                    TransformFn::Matrix(m)
                }
                // Unreachable: discriminants matched above.
                _ => *x,
            })
            .collect()
    } else {
        let ma = collapse(&av);
        let mb = collapse(&bv);
        let mut m = [0.0; 6];
        for i in 0..6 {
            m[i] = lerp_f32(ma[i], mb[i], t);
        }
        vec![TransformFn::Matrix(m)]
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
            lerp_length(&Length::Px(10.0), &Length::Px(20.0), 0.5),
            Length::Px(15.0)
        );
        // mixed → discrete.
        assert_eq!(
            lerp_length(&Length::Px(10.0), &Length::Auto, 0.6),
            Length::Auto
        );
        assert_eq!(
            lerp_length(&Length::Px(10.0), &Length::Auto, 0.4),
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
    fn lerp_transform_rotate_matched() {
        let a = [TransformFn::Rotate(0.0)];
        let b = [TransformFn::Rotate(std::f32::consts::FRAC_PI_2)]; // 90deg
        let out = lerp_transform(&a, &b, 0.5);
        match out.as_slice() {
            [TransformFn::Rotate(r)] => {
                assert!((r - std::f32::consts::FRAC_PI_4).abs() < 1e-5, "{r}")
            }
            other => panic!("expected single Rotate, got {other:?}"),
        }
    }

    #[test]
    fn lerp_transform_translate_matched() {
        let a = [TransformFn::Translate(
            LengthPct::Px(0.0),
            LengthPct::Px(10.0),
        )];
        let b = [TransformFn::Translate(
            LengthPct::Px(100.0),
            LengthPct::Px(30.0),
        )];
        let out = lerp_transform(&a, &b, 0.5);
        assert_eq!(
            out.as_slice(),
            [TransformFn::Translate(
                LengthPct::Px(50.0),
                LengthPct::Px(20.0)
            )]
        );
    }

    #[test]
    fn lerp_transform_mismatched_matrix_fallback() {
        // rotate ↔ scale differ → matrix fallback. At t=0.5 the result is the
        // midpoint of each side's collapsed affine.
        let a = [TransformFn::Rotate(0.0)]; // identity
        let b = [TransformFn::Scale(3.0, 3.0)];
        let out = lerp_transform(&a, &b, 0.5);
        match out.as_slice() {
            [TransformFn::Matrix(m)] => {
                // identity = [1,0,0,1,0,0], scale3 = [3,0,0,3,0,0] → mid [2,0,0,2,0,0].
                assert!((m[0] - 2.0).abs() < 1e-5, "{m:?}");
                assert!((m[3] - 2.0).abs() < 1e-5, "{m:?}");
                assert!(m[1].abs() < 1e-5 && m[2].abs() < 1e-5);
            }
            other => panic!("expected Matrix fallback, got {other:?}"),
        }
    }

    #[test]
    fn lerp_transform_identity_pad() {
        // none ↔ translate: empty list padded with Translate identity.
        let a: [TransformFn; 0] = [];
        let b = [TransformFn::Translate(
            LengthPct::Px(0.0),
            LengthPct::Px(40.0),
        )];
        let out = lerp_transform(&a, &b, 0.5);
        assert_eq!(
            out.as_slice(),
            [TransformFn::Translate(
                LengthPct::Px(0.0),
                LengthPct::Px(20.0)
            )]
        );
    }

    #[test]
    fn lerp_radius_per_corner() {
        let a = [0.0, 10.0, 20.0, 30.0];
        let b = [10.0, 30.0, 20.0, 0.0];
        assert_eq!(lerp_radius(a, b, 0.5), [5.0, 20.0, 20.0, 15.0]);
    }

    #[test]
    fn lerp_box_shadow_some_some_and_none_some() {
        let a = BoxShadow {
            offset_x: 0.0,
            offset_y: 0.0,
            blur: 0.0,
            spread: 0.0,
            color: Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
        };
        let b = BoxShadow {
            offset_x: 10.0,
            offset_y: 20.0,
            blur: 4.0,
            spread: 2.0,
            color: Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255,
            },
        };
        let m = lerp_box_shadow(Some(a), Some(b), 0.5).unwrap();
        assert_eq!(
            (m.offset_x, m.offset_y, m.blur, m.spread),
            (5.0, 10.0, 2.0, 1.0)
        );
        // None ↔ None → None.
        assert_eq!(lerp_box_shadow(None, None, 0.5), None);
        // None ↔ Some: absent side pads with a transparent zero-shadow, so at the
        // midpoint the offsets halve and the alpha is half of `b`'s.
        let m2 = lerp_box_shadow(None, Some(b), 0.5).unwrap();
        assert_eq!((m2.offset_x, m2.offset_y), (5.0, 10.0));
        assert_eq!(m2.color.a, 128);
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
