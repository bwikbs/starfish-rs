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
    if segs[0].trim().to_ascii_lowercase() != "in srgb" {
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
        assert_eq!(parse_color_mix("in oklch, red, blue"), None);
        assert_eq!(parse_color_mix("red, blue"), None);
    }
}
