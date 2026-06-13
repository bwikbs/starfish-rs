//! Small CSS color subset → [`Rgba`]: `#hex`, `rgb()/rgba()`, ~16 named colors.

use crate::model::Rgba;

/// Parse a `#hex` color body (the text after `#`). Supports 3/4/6/8 digit
/// forms. `None` if it isn't valid hex of a supported length.
pub(crate) fn parse_hex(body: &str) -> Option<Rgba> {
    let b = body.as_bytes();
    if !b.iter().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let hx = |c: u8| (c as char).to_digit(16).unwrap() as u8;
    match b.len() {
        3 => Some(Rgba {
            r: hx(b[0]) * 17,
            g: hx(b[1]) * 17,
            b: hx(b[2]) * 17,
            a: 255,
        }),
        4 => Some(Rgba {
            r: hx(b[0]) * 17,
            g: hx(b[1]) * 17,
            b: hx(b[2]) * 17,
            a: hx(b[3]) * 17,
        }),
        6 => Some(Rgba {
            r: hx(b[0]) * 16 + hx(b[1]),
            g: hx(b[2]) * 16 + hx(b[3]),
            b: hx(b[4]) * 16 + hx(b[5]),
            a: 255,
        }),
        8 => Some(Rgba {
            r: hx(b[0]) * 16 + hx(b[1]),
            g: hx(b[2]) * 16 + hx(b[3]),
            b: hx(b[4]) * 16 + hx(b[5]),
            a: hx(b[6]) * 16 + hx(b[7]),
        }),
        _ => None,
    }
}

/// Parse a single CSS color token from verbatim text: `#hex`, `rgb()/rgba()`,
/// or a named color (case-insensitive). `None` if it isn't a color. Used by
/// `starfish_style` to parse colors inside `linear-gradient(...)` raw args,
/// whose contents the tokenizer keeps verbatim. Reuses the helpers below.
pub fn parse_color(token: &str) -> Option<Rgba> {
    let t = token.trim();
    if let Some(body) = t.strip_prefix('#') {
        return parse_hex(body);
    }
    let lower = t.to_ascii_lowercase();
    if lower == "transparent" {
        return Some(Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        });
    }
    if let Some(args) = lower
        .strip_prefix("rgb(")
        .or_else(|| lower.strip_prefix("rgba("))
    {
        let args = args.strip_suffix(')')?;
        return parse_rgb(args);
    }
    if let Some(args) = lower
        .strip_prefix("hsl(")
        .or_else(|| lower.strip_prefix("hsla("))
    {
        let args = args.strip_suffix(')')?;
        return parse_hsl(args);
    }
    // Modern perceptual color functions (E26-M1). Use the ORIGINAL-case token
    // body so component text is intact (lowercasing is harmless here, but keep
    // the un-lowered slice for symmetry with the parser's function dispatch).
    for (prefix, space) in [
        ("oklch(", ModernSpace::Oklch),
        ("oklab(", ModernSpace::Oklab),
        ("lab(", ModernSpace::Lab),
        ("lch(", ModernSpace::Lch),
    ] {
        if let Some(args) = lower.strip_prefix(prefix) {
            let args = args.strip_suffix(')')?;
            return parse_modern_color(space, args);
        }
    }
    named(&lower)
}

/// The ~16 CSS basic named colors. `None` for anything else (kept as keyword).
pub(crate) fn named(name: &str) -> Option<Rgba> {
    let (r, g, b) = match name {
        "black" => (0, 0, 0),
        "white" => (255, 255, 255),
        "red" => (255, 0, 0),
        "green" => (0, 128, 0),
        "blue" => (0, 0, 255),
        "gray" | "grey" => (128, 128, 128),
        "silver" => (192, 192, 192),
        "maroon" => (128, 0, 0),
        "yellow" => (255, 255, 0),
        "olive" => (128, 128, 0),
        "lime" => (0, 255, 0),
        "aqua" => (0, 255, 255),
        "teal" => (0, 128, 128),
        "navy" => (0, 0, 128),
        "fuchsia" => (255, 0, 255),
        "purple" => (128, 0, 128),
        _ => return None,
    };
    Some(Rgba { r, g, b, a: 255 })
}

/// Parse `rgb()/rgba()` raw argument text into [`Rgba`]. Integer or percent
/// channels; alpha is a 0..1 float. `None` on anything unexpected.
pub(crate) fn parse_rgb(raw_args: &str) -> Option<Rgba> {
    let parts: Vec<&str> = raw_args.split(',').map(str::trim).collect();
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let chan = |s: &str| -> Option<u8> {
        if let Some(pct) = s.strip_suffix('%') {
            let v: f32 = pct.trim().parse().ok()?;
            Some((v / 100.0 * 255.0).round().clamp(0.0, 255.0) as u8)
        } else {
            let v: f32 = s.parse().ok()?;
            Some(v.round().clamp(0.0, 255.0) as u8)
        }
    };
    let r = chan(parts[0])?;
    let g = chan(parts[1])?;
    let b = chan(parts[2])?;
    let a = if parts.len() == 4 {
        let v: f32 = parts[3].parse().ok()?;
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    } else {
        255
    };
    Some(Rgba { r, g, b, a })
}

/// Parse `hsl()/hsla()` raw argument text into [`Rgba`] (comma form
/// `h, s%, l% [, a]`). `h` is a number in degrees, `s`/`l` are percentages, and
/// the optional `a` is a 0..1 float (same rounding as `parse_rgb`'s alpha).
/// `None` on anything unexpected (wrong arg count, non-`%` s/l, …).
pub(crate) fn parse_hsl(raw_args: &str) -> Option<Rgba> {
    let parts: Vec<&str> = raw_args.split(',').map(str::trim).collect();
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let h: f32 = parts[0].parse::<f32>().ok()?.rem_euclid(360.0);
    let pct = |s: &str| -> Option<f32> {
        let v: f32 = s.strip_suffix('%')?.trim().parse().ok()?;
        Some((v / 100.0).clamp(0.0, 1.0))
    };
    let s = pct(parts[1])?;
    let l = pct(parts[2])?;
    let (r, g, b) = hsl_to_rgb(h, s, l);
    let a = if parts.len() == 4 {
        let v: f32 = parts[3].parse().ok()?;
        (v * 255.0).round().clamp(0.0, 255.0) as u8
    } else {
        255
    };
    Some(Rgba { r, g, b, a })
}

/// Which perceptual color function a set of args belongs to (E26-M1).
#[derive(Clone, Copy)]
pub(crate) enum ModernSpace {
    Oklch,
    Oklab,
    Lab,
    Lch,
}

/// Map a lowercased function name to its [`ModernSpace`], if any (E26-M1).
pub(crate) fn modern_space(name: &str) -> Option<ModernSpace> {
    match name {
        "oklch" => Some(ModernSpace::Oklch),
        "oklab" => Some(ModernSpace::Oklab),
        "lab" => Some(ModernSpace::Lab),
        "lch" => Some(ModernSpace::Lch),
        _ => None,
    }
}

/// Parse a modern color function's raw args (`L C H [/ A]` etc.) to sRGB
/// [`Rgba`] (E26-M1). Components are space-separated with an optional `/ alpha`.
/// `None` on the wrong component count or an unparseable token.
pub(crate) fn parse_modern_color(space: ModernSpace, raw_args: &str) -> Option<Rgba> {
    let (main, alpha_s) = match raw_args.split_once('/') {
        Some((m, a)) => (m, Some(a)),
        None => (raw_args, None),
    };
    let parts: Vec<&str> = main.split_whitespace().collect();
    if parts.len() != 3 {
        return None;
    }
    let alpha = match alpha_s {
        Some(a) => parse_alpha(a.trim())?,
        None => 255,
    };
    // Per-space component scales: (L-full, C/a-full, b-full-for-rect).
    let (c1, c2, c3) = match space {
        // OKLab/OKLCh L is 0..1; chroma/ab full-scale 0.4.
        ModernSpace::Oklch => {
            let l = num_or_pct(parts[0], 1.0)?;
            let c = num_or_pct(parts[1], 0.4)?;
            let h = parse_angle(parts[2])?;
            return oklab_to_rgba(l, c * h.to_radians().cos(), c * h.to_radians().sin(), alpha);
        }
        ModernSpace::Oklab => (
            num_or_pct(parts[0], 1.0)?,
            num_or_pct(parts[1], 0.4)?,
            num_or_pct(parts[2], 0.4)?,
        ),
        // CIE Lab L is 0..100; a/b full-scale 125; LCh chroma full-scale 150.
        ModernSpace::Lab => (
            num_or_pct(parts[0], 100.0)?,
            num_or_pct(parts[1], 125.0)?,
            num_or_pct(parts[2], 125.0)?,
        ),
        ModernSpace::Lch => {
            let l = num_or_pct(parts[0], 100.0)?;
            let c = num_or_pct(parts[1], 150.0)?;
            let h = parse_angle(parts[2])?;
            return lab_to_rgba(l, c * h.to_radians().cos(), c * h.to_radians().sin(), alpha);
        }
    };
    match space {
        ModernSpace::Oklab => oklab_to_rgba(c1, c2, c3, alpha),
        ModernSpace::Lab => lab_to_rgba(c1, c2, c3, alpha),
        _ => unreachable!(),
    }
}

/// Parse a number or percentage token; `%` scales by `full` (100% → `full`).
fn num_or_pct(s: &str, full: f32) -> Option<f32> {
    let s = s.trim();
    if let Some(p) = s.strip_suffix('%') {
        Some(p.trim().parse::<f32>().ok()? / 100.0 * full)
    } else {
        s.parse::<f32>().ok()
    }
}

/// Parse an angle: a bare number or a `deg` value, in degrees.
fn parse_angle(s: &str) -> Option<f32> {
    let s = s.trim();
    let s = s.strip_suffix("deg").unwrap_or(s);
    s.trim().parse::<f32>().ok()
}

/// Parse an alpha token (`0..1` number or a percentage) to 0..255.
fn parse_alpha(s: &str) -> Option<u8> {
    let v = if let Some(p) = s.strip_suffix('%') {
        p.trim().parse::<f32>().ok()? / 100.0
    } else {
        s.parse::<f32>().ok()?
    };
    Some((v * 255.0).round().clamp(0.0, 255.0) as u8)
}

/// Encode a linear-light sRGB channel (0..1) to an 8-bit gamma sRGB value.
fn encode_srgb(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.0031308 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

/// OKLab → sRGB [`Rgba`] (CSS Color 4 matrices: OKLab → LMS → linear sRGB).
fn oklab_to_rgba(l: f32, a: f32, b: f32, alpha: u8) -> Option<Rgba> {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let lr = 4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3;
    let lg = -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3;
    let lb = -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3;
    Some(Rgba {
        r: encode_srgb(lr),
        g: encode_srgb(lg),
        b: encode_srgb(lb),
        a: alpha,
    })
}

/// CIE Lab (D50) → sRGB [`Rgba`] (Lab → XYZ D50 → linear sRGB → gamma).
fn lab_to_rgba(l: f32, a: f32, b: f32, alpha: u8) -> Option<Rgba> {
    const KAPPA: f32 = 24389.0 / 27.0;
    const EPS: f32 = 216.0 / 24389.0;
    let fy = (l + 16.0) / 116.0;
    let fx = fy + a / 500.0;
    let fz = fy - b / 200.0;
    let f_inv = |t: f32| {
        let t3 = t * t * t;
        if t3 > EPS {
            t3
        } else {
            (116.0 * t - 16.0) / KAPPA
        }
    };
    // D50 reference white.
    let (xn, yn, zn) = (0.964_22, 1.0, 0.825_21);
    let x = f_inv(fx) * xn;
    let y = f_inv(fy) * yn;
    let z = f_inv(fz) * zn;
    // XYZ (D50) → linear sRGB (CSS Color 4 composed Bradford-adapted matrix).
    let lr = 3.134_136 * x - 1.617_386 * y - 0.490_662 * z;
    let lg = -0.978_795 * x + 1.916_142 * y + 0.033_454 * z;
    let lb = 0.071_959_4 * x - 0.228_994 * y + 1.405_248_6 * z;
    Some(Rgba {
        r: encode_srgb(lr),
        g: encode_srgb(lg),
        b: encode_srgb(lb),
        a: alpha,
    })
}

/// Parse `color-mix(in srgb, A [p%], B [q%])` raw argument text into the mixed
/// [`Rgba`] (E24-M3). Only the `in srgb` color space is supported; mixing is
/// done in gamma-encoded sRGB with premultiplied alpha (the CSS Color 5 srgb
/// recipe). `None` on a different color space, wrong arg count, or unparseable
/// colors.
pub(crate) fn parse_color_mix(raw_args: &str) -> Option<Rgba> {
    let segs = split_top_level_commas(raw_args);
    if segs.len() != 3 {
        return None;
    }
    if !segs[0].trim().eq_ignore_ascii_case("in srgb") {
        return None;
    }
    let (c1, p1) = parse_color_with_pct(&segs[1])?;
    let (c2, p2) = parse_color_with_pct(&segs[2])?;
    Some(mix_srgb(c1, p1, c2, p2))
}

/// Split `s` on top-level commas (ignoring commas inside parens, so nested
/// `rgb(0, 0, 255)` stays one segment).
fn split_top_level_commas(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    out.push(cur.trim().to_string());
    out
}

/// Parse a `<color> [<percentage>]` segment for `color-mix`. The percentage is
/// the trailing whitespace-separated `N%` token (if any); the rest is a color.
fn parse_color_with_pct(seg: &str) -> Option<(Rgba, Option<f32>)> {
    let s = seg.trim();
    if let Some((rest, last)) = s.rsplit_once(char::is_whitespace) {
        if let Some(num) = last.strip_suffix('%') {
            if let Ok(v) = num.trim().parse::<f32>() {
                return Some((parse_color(rest.trim())?, Some(v)));
            }
        }
    }
    Some((parse_color(s)?, None))
}

/// Mix two sRGB colors by weight (percentages, default 50/50). One percentage
/// given implies the other is `100 - p`. Mixing is premultiplied; an original
/// weight sum below 100% multiplies the result's alpha (CSS Color 5).
fn mix_srgb(c1: Rgba, p1: Option<f32>, c2: Rgba, p2: Option<f32>) -> Rgba {
    let (w1, w2) = match (p1, p2) {
        (None, None) => (50.0, 50.0),
        (Some(a), None) => (a, 100.0 - a),
        (None, Some(b)) => (100.0 - b, b),
        (Some(a), Some(b)) => (a, b),
    };
    let sum = w1 + w2;
    if sum <= 0.0 {
        return Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: 0,
        };
    }
    let alpha_mult = if sum < 100.0 { sum / 100.0 } else { 1.0 };
    let (n1, n2) = (w1 / sum, w2 / sum);
    let (a1, a2) = (c1.a as f32 / 255.0, c2.a as f32 / 255.0);
    let am = n1 * a1 + n2 * a2;
    let chan = |x1: u8, x2: u8| -> u8 {
        if am <= 0.0 {
            return 0;
        }
        ((n1 * a1 * x1 as f32 + n2 * a2 * x2 as f32) / am)
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgba {
        r: chan(c1.r, c2.r),
        g: chan(c1.g, c2.g),
        b: chan(c1.b, c2.b),
        a: (am * alpha_mult * 255.0).round().clamp(0.0, 255.0) as u8,
    }
}

/// Standard CSS Color HSL→RGB. `h` in degrees [0,360), `s`/`l` in [0,1].
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let ch = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (ch(r1), ch(g1), ch(b1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_keyword() {
        assert_eq!(
            parse_color("transparent"),
            Some(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0
            })
        );
        // case-insensitive, with surrounding whitespace.
        assert_eq!(
            parse_color("  TRANSPARENT  "),
            Some(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0
            })
        );
    }

    #[test]
    fn color_mix_red_blue_is_purple() {
        // 50/50 red+blue in srgb → (128, 0, 128).
        assert_eq!(
            parse_color_mix("in srgb, red, blue"),
            Some(Rgba {
                r: 128,
                g: 0,
                b: 128,
                a: 255
            })
        );
    }

    #[test]
    fn color_mix_weighted_and_nested() {
        // 100% one side wins; nested rgb() with internal commas stays one seg.
        assert_eq!(
            parse_color_mix("in srgb, red 100%, blue"),
            Some(Rgba {
                r: 255,
                g: 0,
                b: 0,
                a: 255
            })
        );
        assert_eq!(
            parse_color_mix("in srgb, rgb(0, 0, 255), white"),
            Some(Rgba {
                r: 128,
                g: 128,
                b: 255,
                a: 255
            })
        );
    }

    #[test]
    fn color_mix_rejects_other_spaces() {
        assert_eq!(parse_color_mix("red, blue"), None);
    }

    fn close_tol(a: Option<Rgba>, r: u8, g: u8, b: u8, tol: i32) {
        let c = a.expect("a color");
        let near = |x: u8, y: u8| (x as i32 - y as i32).abs() <= tol;
        assert!(
            near(c.r, r) && near(c.g, g) && near(c.b, b),
            "got {c:?}, want ~({r},{g},{b})"
        );
    }
    fn close(a: Option<Rgba>, r: u8, g: u8, b: u8) {
        close_tol(a, r, g, b, 2);
    }

    #[test]
    fn oklch_white_black_red() {
        // L=1, C=0 → white; L=0 → black; sRGB red ≈ oklch(0.628 0.2577 29.23).
        close(parse_modern_color(ModernSpace::Oklch, "1 0 0"), 255, 255, 255);
        close(parse_modern_color(ModernSpace::Oklab, "0 0 0"), 0, 0, 0);
        close(parse_modern_color(ModernSpace::Oklch, "0.628 0.2577 29.23"), 255, 0, 0);
    }

    #[test]
    fn lab_white_and_red() {
        close(parse_modern_color(ModernSpace::Lab, "100 0 0"), 255, 255, 255);
        // CIE Lab of sRGB red ≈ (53.24, 80.09, 67.20); D50 matrix rounding makes
        // the round-trip land within a few levels of pure red.
        close_tol(parse_modern_color(ModernSpace::Lab, "53.24 80.09 67.20"), 255, 0, 0, 8);
        // percentage L (50% → L=50) parses.
        assert!(parse_modern_color(ModernSpace::Lab, "50% 40 30").is_some());
    }

    #[test]
    fn modern_color_alpha_and_via_parse_color() {
        // `/ alpha` and dispatch through parse_color (verbatim token).
        let c = parse_modern_color(ModernSpace::Oklch, "1 0 0 / 0.5").unwrap();
        assert_eq!(c.a, 128);
        close(parse_color("oklch(1 0 0)"), 255, 255, 255);
    }
}
