//! Small CSS color subset → [`Rgba`]: `#hex`, `rgb()/rgba()`, named colors.

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

/// The full CSS Color 4 `<named-color>` set (148 keywords incl. `rebeccapurple`;
/// `gray`/`grey` aliases both present). Looked up case-insensitively via a sorted
/// table + binary search; `None` for anything else (kept as keyword).
/// `transparent` is handled by the callers, not here. (E44-M1)
pub(crate) fn named(name: &str) -> Option<Rgba> {
    NAMED_COLORS
        .binary_search_by(|(n, _)| n.cmp(&name))
        .ok()
        .map(|i| {
            let (r, g, b) = NAMED_COLORS[i].1;
            Rgba { r, g, b, a: 255 }
        })
}

/// Sorted `(name, (r, g, b))` table of the CSS named colors (E44-M1). Must stay
/// lexicographically sorted by name for `named()`'s binary search.
#[rustfmt::skip]
static NAMED_COLORS: &[(&str, (u8, u8, u8))] = &[
    ("aliceblue", (240, 248, 255)),
    ("antiquewhite", (250, 235, 215)),
    ("aqua", (0, 255, 255)),
    ("aquamarine", (127, 255, 212)),
    ("azure", (240, 255, 255)),
    ("beige", (245, 245, 220)),
    ("bisque", (255, 228, 196)),
    ("black", (0, 0, 0)),
    ("blanchedalmond", (255, 235, 205)),
    ("blue", (0, 0, 255)),
    ("blueviolet", (138, 43, 226)),
    ("brown", (165, 42, 42)),
    ("burlywood", (222, 184, 135)),
    ("cadetblue", (95, 158, 160)),
    ("chartreuse", (127, 255, 0)),
    ("chocolate", (210, 105, 30)),
    ("coral", (255, 127, 80)),
    ("cornflowerblue", (100, 149, 237)),
    ("cornsilk", (255, 248, 220)),
    ("crimson", (220, 20, 60)),
    ("cyan", (0, 255, 255)),
    ("darkblue", (0, 0, 139)),
    ("darkcyan", (0, 139, 139)),
    ("darkgoldenrod", (184, 134, 11)),
    ("darkgray", (169, 169, 169)),
    ("darkgreen", (0, 100, 0)),
    ("darkgrey", (169, 169, 169)),
    ("darkkhaki", (189, 183, 107)),
    ("darkmagenta", (139, 0, 139)),
    ("darkolivegreen", (85, 107, 47)),
    ("darkorange", (255, 140, 0)),
    ("darkorchid", (153, 50, 204)),
    ("darkred", (139, 0, 0)),
    ("darksalmon", (233, 150, 122)),
    ("darkseagreen", (143, 188, 143)),
    ("darkslateblue", (72, 61, 139)),
    ("darkslategray", (47, 79, 79)),
    ("darkslategrey", (47, 79, 79)),
    ("darkturquoise", (0, 206, 209)),
    ("darkviolet", (148, 0, 211)),
    ("deeppink", (255, 20, 147)),
    ("deepskyblue", (0, 191, 255)),
    ("dimgray", (105, 105, 105)),
    ("dimgrey", (105, 105, 105)),
    ("dodgerblue", (30, 144, 255)),
    ("firebrick", (178, 34, 34)),
    ("floralwhite", (255, 250, 240)),
    ("forestgreen", (34, 139, 34)),
    ("fuchsia", (255, 0, 255)),
    ("gainsboro", (220, 220, 220)),
    ("ghostwhite", (248, 248, 255)),
    ("gold", (255, 215, 0)),
    ("goldenrod", (218, 165, 32)),
    ("gray", (128, 128, 128)),
    ("green", (0, 128, 0)),
    ("greenyellow", (173, 255, 47)),
    ("grey", (128, 128, 128)),
    ("honeydew", (240, 255, 240)),
    ("hotpink", (255, 105, 180)),
    ("indianred", (205, 92, 92)),
    ("indigo", (75, 0, 130)),
    ("ivory", (255, 255, 240)),
    ("khaki", (240, 230, 140)),
    ("lavender", (230, 230, 250)),
    ("lavenderblush", (255, 240, 245)),
    ("lawngreen", (124, 252, 0)),
    ("lemonchiffon", (255, 250, 205)),
    ("lightblue", (173, 216, 230)),
    ("lightcoral", (240, 128, 128)),
    ("lightcyan", (224, 255, 255)),
    ("lightgoldenrodyellow", (250, 250, 210)),
    ("lightgray", (211, 211, 211)),
    ("lightgreen", (144, 238, 144)),
    ("lightgrey", (211, 211, 211)),
    ("lightpink", (255, 182, 193)),
    ("lightsalmon", (255, 160, 122)),
    ("lightseagreen", (32, 178, 170)),
    ("lightskyblue", (135, 206, 250)),
    ("lightslategray", (119, 136, 153)),
    ("lightslategrey", (119, 136, 153)),
    ("lightsteelblue", (176, 196, 222)),
    ("lightyellow", (255, 255, 224)),
    ("lime", (0, 255, 0)),
    ("limegreen", (50, 205, 50)),
    ("linen", (250, 240, 230)),
    ("magenta", (255, 0, 255)),
    ("maroon", (128, 0, 0)),
    ("mediumaquamarine", (102, 205, 170)),
    ("mediumblue", (0, 0, 205)),
    ("mediumorchid", (186, 85, 211)),
    ("mediumpurple", (147, 112, 219)),
    ("mediumseagreen", (60, 179, 113)),
    ("mediumslateblue", (123, 104, 238)),
    ("mediumspringgreen", (0, 250, 154)),
    ("mediumturquoise", (72, 209, 204)),
    ("mediumvioletred", (199, 21, 133)),
    ("midnightblue", (25, 25, 112)),
    ("mintcream", (245, 255, 250)),
    ("mistyrose", (255, 228, 225)),
    ("moccasin", (255, 228, 181)),
    ("navajowhite", (255, 222, 173)),
    ("navy", (0, 0, 128)),
    ("oldlace", (253, 245, 230)),
    ("olive", (128, 128, 0)),
    ("olivedrab", (107, 142, 35)),
    ("orange", (255, 165, 0)),
    ("orangered", (255, 69, 0)),
    ("orchid", (218, 112, 214)),
    ("palegoldenrod", (238, 232, 170)),
    ("palegreen", (152, 251, 152)),
    ("paleturquoise", (175, 238, 238)),
    ("palevioletred", (219, 112, 147)),
    ("papayawhip", (255, 239, 213)),
    ("peachpuff", (255, 218, 185)),
    ("peru", (205, 133, 63)),
    ("pink", (255, 192, 203)),
    ("plum", (221, 160, 221)),
    ("powderblue", (176, 224, 230)),
    ("purple", (128, 0, 128)),
    ("rebeccapurple", (102, 51, 153)),
    ("red", (255, 0, 0)),
    ("rosybrown", (188, 143, 143)),
    ("royalblue", (65, 105, 225)),
    ("saddlebrown", (139, 69, 19)),
    ("salmon", (250, 128, 114)),
    ("sandybrown", (244, 164, 96)),
    ("seagreen", (46, 139, 87)),
    ("seashell", (255, 245, 238)),
    ("sienna", (160, 82, 45)),
    ("silver", (192, 192, 192)),
    ("skyblue", (135, 206, 235)),
    ("slateblue", (106, 90, 205)),
    ("slategray", (112, 128, 144)),
    ("slategrey", (112, 128, 144)),
    ("snow", (255, 250, 250)),
    ("springgreen", (0, 255, 127)),
    ("steelblue", (70, 130, 180)),
    ("tan", (210, 180, 140)),
    ("teal", (0, 128, 128)),
    ("thistle", (216, 191, 216)),
    ("tomato", (255, 99, 71)),
    ("turquoise", (64, 224, 208)),
    ("violet", (238, 130, 238)),
    ("wheat", (245, 222, 179)),
    ("white", (255, 255, 255)),
    ("whitesmoke", (245, 245, 245)),
    ("yellow", (255, 255, 0)),
    ("yellowgreen", (154, 205, 50)),
];

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

/// `light-dark(a, b)` → its first (light-scheme) argument (E26-M3). The engine
/// has no color-scheme toggle, so the light value always wins.
pub(crate) fn parse_light_dark(raw_args: &str) -> Option<Rgba> {
    let segs = split_top_level_commas(raw_args);
    if segs.len() != 2 {
        return None;
    }
    parse_color(segs[0].trim())
}

/// Parse a relative color function body, `from <color> <c1> <c2> <c3> [/ <a>]`,
/// for `func` ∈ rgb/hsl/oklch/oklab/lab/lch (E26-M3). The origin color is
/// decomposed into `func`'s channels, exposed by name; each output channel is a
/// channel keyword, a number, a percentage, or a `calc()` over those. `None` if
/// it isn't a `from` form or a component fails to parse.
pub(crate) fn parse_relative_color(func: &str, raw_args: &str) -> Option<Rgba> {
    let body = raw_args.trim();
    let rest = body
        .strip_prefix("from ")
        .or_else(|| body.strip_prefix("FROM "))?
        .trim_start();
    let (origin_tok, rest) = rest.split_once(char::is_whitespace)?;
    let origin = parse_color(origin_tok.trim())?;

    // Decompose the origin into func-space channels (name → value) + alpha 0..1,
    // plus each output channel's percentage full-scale.
    let alpha01 = origin.a as f32 / 255.0;
    let (chans, scales): (Vec<(&str, f32)>, [f32; 3]) = match func {
        "rgb" => {
            let v = vec![
                ("r", origin.r as f32),
                ("g", origin.g as f32),
                ("b", origin.b as f32),
                ("alpha", alpha01),
            ];
            (v, [255.0, 255.0, 255.0])
        }
        "hsl" => {
            let [h, s, l] = rgba_to_hsl(origin);
            (
                vec![
                    ("h", h),
                    ("s", s * 100.0),
                    ("l", l * 100.0),
                    ("alpha", alpha01),
                ],
                [360.0, 100.0, 100.0],
            )
        }
        "oklab" => {
            let [l, a, b] = rgba_to_oklab(origin);
            (
                vec![("l", l), ("a", a), ("b", b), ("alpha", alpha01)],
                [1.0, 0.4, 0.4],
            )
        }
        "oklch" => {
            let [l, c, h] = to_polar(rgba_to_oklab(origin));
            (
                vec![("l", l), ("c", c), ("h", h), ("alpha", alpha01)],
                [1.0, 0.4, 360.0],
            )
        }
        "lab" => {
            let [l, a, b] = rgba_to_lab(origin);
            (
                vec![("l", l), ("a", a), ("b", b), ("alpha", alpha01)],
                [100.0, 125.0, 125.0],
            )
        }
        "lch" => {
            let [l, c, h] = to_polar(rgba_to_lab(origin));
            (
                vec![("l", l), ("c", c), ("h", h), ("alpha", alpha01)],
                [100.0, 150.0, 360.0],
            )
        }
        _ => return None,
    };

    let (main, alpha_s) = match rest.split_once('/') {
        Some((m, a)) => (m, Some(a)),
        None => (rest, None),
    };
    let toks = split_ws_top(main);
    if toks.len() != 3 {
        return None;
    }
    let c0 = eval_channel(toks[0], &chans, scales[0])?;
    let c1 = eval_channel(toks[1], &chans, scales[1])?;
    let c2 = eval_channel(toks[2], &chans, scales[2])?;
    let alpha = match alpha_s {
        Some(a) => (eval_channel(a.trim(), &chans, 1.0)? * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8,
        None => origin.a,
    };

    let ch = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    match func {
        "rgb" => Some(Rgba {
            r: ch(c0),
            g: ch(c1),
            b: ch(c2),
            a: alpha,
        }),
        "hsl" => {
            let (r, g, b) = hsl_to_rgb(
                c0.rem_euclid(360.0),
                (c1 / 100.0).clamp(0.0, 1.0),
                (c2 / 100.0).clamp(0.0, 1.0),
            );
            Some(Rgba { r, g, b, a: alpha })
        }
        "oklab" => oklab_to_rgba(c0, c1, c2, alpha),
        "oklch" => {
            let [l, a, b] = from_polar([c0, c1, c2]);
            oklab_to_rgba(l, a, b, alpha)
        }
        "lab" => lab_to_rgba(c0, c1, c2, alpha),
        "lch" => {
            let [l, a, b] = from_polar([c0, c1, c2]);
            lab_to_rgba(l, a, b, alpha)
        }
        _ => None,
    }
}

/// Split on top-level whitespace, keeping parenthesized groups (`calc(l * 0.5)`)
/// as one token.
fn split_ws_top(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut last = 0usize;
    for (idx, ch) in s.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if idx > last {
                    out.push(&s[last..idx]);
                }
                last = idx + ch.len_utf8();
            }
            _ => {}
        }
    }
    if last < s.len() {
        out.push(&s[last..]);
    }
    out.into_iter().filter(|t| !t.is_empty()).collect()
}

/// Evaluate one relative-color channel expression: a channel keyword, number,
/// percentage (scaled by `full`), or a two-operand `calc()` over those.
fn eval_channel(expr: &str, chans: &[(&str, f32)], full: f32) -> Option<f32> {
    let e = expr.trim();
    let inner = e
        .strip_prefix("calc(")
        .and_then(|x| x.strip_suffix(')'))
        .map(str::trim)
        .unwrap_or(e);
    let lookup = |t: &str| -> Option<f32> {
        let t = t.trim();
        if let Some((_, v)) = chans.iter().find(|(n, _)| n.eq_ignore_ascii_case(t)) {
            Some(*v)
        } else if let Some(p) = t.strip_suffix('%') {
            Some(p.trim().parse::<f32>().ok()? / 100.0 * full)
        } else {
            t.parse::<f32>().ok()
        }
    };
    let parts: Vec<&str> = inner.split_whitespace().collect();
    match parts.as_slice() {
        [a] => lookup(a),
        [a, op, b] => {
            let (x, y) = (lookup(a)?, lookup(b)?);
            Some(match *op {
                "*" => x * y,
                "/" => x / y,
                "+" => x + y,
                "-" => x - y,
                _ => return None,
            })
        }
        _ => None,
    }
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

/// Decode an 8-bit gamma sRGB channel to linear light (0..1).
fn decode_srgb(u: u8) -> f32 {
    let c = u as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB [`Rgba`] → OKLab `[L, a, b]`.
fn rgba_to_oklab(c: Rgba) -> [f32; 3] {
    let (r, g, b) = (decode_srgb(c.r), decode_srgb(c.g), decode_srgb(c.b));
    let l = 0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b;
    let m = 0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b;
    let s = 0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    [
        0.210_454_26 * l_ + 0.793_617_8 * m_ - 0.004_072_047 * s_,
        1.977_998_5 * l_ - 2.428_592_2 * m_ + 0.450_593_7 * s_,
        0.025_904_037 * l_ + 0.782_771_77 * m_ - 0.808_675_77 * s_,
    ]
}

/// sRGB [`Rgba`] → CIE Lab (D50) `[L, a, b]`.
fn rgba_to_lab(c: Rgba) -> [f32; 3] {
    let (r, g, b) = (decode_srgb(c.r), decode_srgb(c.g), decode_srgb(c.b));
    let x = 0.436_074_7 * r + 0.385_064_9 * g + 0.143_080_4 * b;
    let y = 0.222_504_5 * r + 0.716_878_6 * g + 0.060_616_9 * b;
    let z = 0.013_932_2 * r + 0.097_104_5 * g + 0.714_173_3 * b;
    const EPS: f32 = 216.0 / 24389.0;
    const KAPPA: f32 = 24389.0 / 27.0;
    let f = |t: f32| {
        if t > EPS {
            t.cbrt()
        } else {
            (KAPPA * t + 16.0) / 116.0
        }
    };
    let (fx, fy, fz) = (f(x / 0.964_22), f(y), f(z / 0.825_21));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// sRGB [`Rgba`] → HSL `[H(deg), S(0..1), L(0..1)]`.
fn rgba_to_hsl(c: Rgba) -> [f32; 3] {
    let (r, g, b) = (c.r as f32 / 255.0, c.g as f32 / 255.0, c.b as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d.abs() < 1e-6 {
        return [0.0, 0.0, l];
    }
    let s = d / (1.0 - (2.0 * l - 1.0).abs());
    let h = if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    [h.rem_euclid(360.0), s, l]
}

/// A `color-mix` interpolation space (E26-M2).
#[derive(Clone, Copy, PartialEq)]
enum MixSpace {
    Srgb,
    Oklab,
    Oklch,
    Lab,
    Lch,
    Hsl,
}

/// A hue-interpolation method for polar spaces (E26-M2).
#[derive(Clone, Copy)]
enum HueMethod {
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

/// Parse `color-mix(in <space> [<hue> hue], A [p%], B [q%])` to the mixed
/// [`Rgba`] (E24-M3 srgb; E26-M2 oklch/oklab/lab/lch/hsl + hue methods). Mixing
/// is premultiplied by alpha; polar spaces interpolate hue per the method
/// (default `shorter`). `None` on a bad space, arg count, or unparseable colors.
pub(crate) fn parse_color_mix(raw_args: &str) -> Option<Rgba> {
    let segs = split_top_level_commas(raw_args);
    if segs.len() != 3 {
        return None;
    }
    let (space, hue) = parse_mix_prelude(segs[0].trim())?;
    let (c1, p1) = parse_color_with_pct(&segs[1])?;
    let (c2, p2) = parse_color_with_pct(&segs[2])?;
    if space == MixSpace::Srgb {
        return Some(mix_srgb(c1, p1, c2, p2));
    }
    Some(mix_in_space(space, hue, c1, p1, c2, p2))
}

/// Parse the `in <space> [<hue> hue]` prelude.
fn parse_mix_prelude(s: &str) -> Option<(MixSpace, HueMethod)> {
    let lower = s.to_ascii_lowercase();
    let rest = lower.strip_prefix("in ")?.trim();
    let mut it = rest.split_whitespace();
    let space = match it.next()? {
        "srgb" => MixSpace::Srgb,
        "oklab" => MixSpace::Oklab,
        "oklch" => MixSpace::Oklch,
        "lab" => MixSpace::Lab,
        "lch" => MixSpace::Lch,
        "hsl" => MixSpace::Hsl,
        _ => return None,
    };
    let hue = match it.next() {
        Some("longer") => HueMethod::Longer,
        Some("increasing") => HueMethod::Increasing,
        Some("decreasing") => HueMethod::Decreasing,
        _ => HueMethod::Shorter,
    };
    Some((space, hue))
}

/// Mix two colors in a non-srgb space (E26-M2).
fn mix_in_space(
    space: MixSpace,
    hue: HueMethod,
    c1: Rgba,
    p1: Option<f32>,
    c2: Rgba,
    p2: Option<f32>,
) -> Rgba {
    let (w1, w2) = mix_weights(p1, p2);
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

    let (k1, k2, hue_idx) = decompose(space, c1, c2);
    let mut out = [0.0f32; 3];
    for i in 0..3 {
        out[i] = if Some(i) == hue_idx {
            interp_hue(k1[i], k2[i], n2, hue)
        } else if am <= 0.0 {
            0.0
        } else {
            (n1 * a1 * k1[i] + n2 * a2 * k2[i]) / am
        };
    }
    let alpha = (am * alpha_mult * 255.0).round().clamp(0.0, 255.0) as u8;
    recompose(space, out, alpha)
}

/// Decompose both colors into `space`'s components; also report which component
/// index is the (polar) hue, if any.
fn decompose(space: MixSpace, c1: Rgba, c2: Rgba) -> ([f32; 3], [f32; 3], Option<usize>) {
    match space {
        MixSpace::Oklab => (rgba_to_oklab(c1), rgba_to_oklab(c2), None),
        MixSpace::Lab => (rgba_to_lab(c1), rgba_to_lab(c2), None),
        MixSpace::Oklch => (
            to_polar(rgba_to_oklab(c1)),
            to_polar(rgba_to_oklab(c2)),
            Some(2),
        ),
        MixSpace::Lch => (
            to_polar(rgba_to_lab(c1)),
            to_polar(rgba_to_lab(c2)),
            Some(2),
        ),
        MixSpace::Hsl => (rgba_to_hsl(c1), rgba_to_hsl(c2), Some(0)),
        MixSpace::Srgb => unreachable!(),
    }
}

/// Recompose `space`'s components (+ alpha) back to sRGB [`Rgba`].
fn recompose(space: MixSpace, k: [f32; 3], alpha: u8) -> Rgba {
    let or_black = |o: Option<Rgba>| {
        o.unwrap_or(Rgba {
            r: 0,
            g: 0,
            b: 0,
            a: alpha,
        })
    };
    match space {
        MixSpace::Oklab => or_black(oklab_to_rgba(k[0], k[1], k[2], alpha)),
        MixSpace::Lab => or_black(lab_to_rgba(k[0], k[1], k[2], alpha)),
        MixSpace::Oklch => {
            let [l, a, b] = from_polar(k);
            or_black(oklab_to_rgba(l, a, b, alpha))
        }
        MixSpace::Lch => {
            let [l, a, b] = from_polar(k);
            or_black(lab_to_rgba(l, a, b, alpha))
        }
        MixSpace::Hsl => {
            let (r, g, b) = hsl_to_rgb(
                k[0].rem_euclid(360.0),
                k[1].clamp(0.0, 1.0),
                k[2].clamp(0.0, 1.0),
            );
            Rgba { r, g, b, a: alpha }
        }
        MixSpace::Srgb => unreachable!(),
    }
}

/// `[L, a, b]` → `[L, C, H(deg)]`.
fn to_polar(lab: [f32; 3]) -> [f32; 3] {
    let c = (lab[1] * lab[1] + lab[2] * lab[2]).sqrt();
    let h = lab[2].atan2(lab[1]).to_degrees().rem_euclid(360.0);
    [lab[0], c, h]
}

/// `[L, C, H(deg)]` → `[L, a, b]`.
fn from_polar(lch: [f32; 3]) -> [f32; 3] {
    let hr = lch[2].to_radians();
    [lch[0], lch[1] * hr.cos(), lch[1] * hr.sin()]
}

/// Interpolate hue (deg) by `t` (weight of the 2nd color) per the method.
fn interp_hue(h1: f32, h2: f32, t: f32, method: HueMethod) -> f32 {
    let mut a = h1.rem_euclid(360.0);
    let mut b = h2.rem_euclid(360.0);
    let d = b - a;
    match method {
        HueMethod::Shorter => {
            if d > 180.0 {
                a += 360.0;
            } else if d < -180.0 {
                b += 360.0;
            }
        }
        HueMethod::Longer => {
            if (0.0..=180.0).contains(&d) {
                a += 360.0;
            } else if d > -180.0 && d <= 0.0 {
                b += 360.0;
            }
        }
        HueMethod::Increasing => {
            if d < 0.0 {
                b += 360.0;
            }
        }
        HueMethod::Decreasing => {
            if d > 0.0 {
                a += 360.0;
            }
        }
    }
    (a + t * (b - a)).rem_euclid(360.0)
}

/// Resolve the two percentage weights (default 50/50; one given ⇒ `100-p`).
fn mix_weights(p1: Option<f32>, p2: Option<f32>) -> (f32, f32) {
    match (p1, p2) {
        (None, None) => (50.0, 50.0),
        (Some(a), None) => (a, 100.0 - a),
        (None, Some(b)) => (100.0 - b, b),
        (Some(a), Some(b)) => (a, b),
    }
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
        close(
            parse_modern_color(ModernSpace::Oklch, "1 0 0"),
            255,
            255,
            255,
        );
        close(parse_modern_color(ModernSpace::Oklab, "0 0 0"), 0, 0, 0);
        close(
            parse_modern_color(ModernSpace::Oklch, "0.628 0.2577 29.23"),
            255,
            0,
            0,
        );
    }

    #[test]
    fn lab_white_and_red() {
        close(
            parse_modern_color(ModernSpace::Lab, "100 0 0"),
            255,
            255,
            255,
        );
        // CIE Lab of sRGB red ≈ (53.24, 80.09, 67.20); D50 matrix rounding makes
        // the round-trip land within a few levels of pure red.
        close_tol(
            parse_modern_color(ModernSpace::Lab, "53.24 80.09 67.20"),
            255,
            0,
            0,
            8,
        );
        // percentage L (50% → L=50) parses.
        assert!(parse_modern_color(ModernSpace::Lab, "50% 40 30").is_some());
    }

    #[test]
    fn color_mix_oklch_differs_from_srgb() {
        let ok = parse_color_mix("in oklch, white, black").unwrap();
        let srgb = parse_color_mix("in srgb, white, black").unwrap();
        // Perceptual midpoint is darker than the gamma-srgb 50% gray.
        assert_ne!(ok, srgb);
        assert!(ok.r < srgb.r);
    }

    #[test]
    fn color_mix_longer_hue_goes_around() {
        // Same hue + longer ⇒ the interpolation travels 360°, landing opposite.
        let c = parse_color_mix("in oklch longer hue, red, red").unwrap();
        assert_ne!(c, named("red").unwrap());
    }

    #[test]
    fn color_mix_srgb_still_purple() {
        // E24-M3 behavior preserved.
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
    fn color_mix_hsl_space() {
        // hsl mix of red+lime sits on the hue circle (yellow-ish), not the srgb avg.
        let c = parse_color_mix("in hsl, red, lime").unwrap();
        assert!(c.r > 100 && c.g > 100);
    }

    #[test]
    fn relative_rgb_drop_and_keep() {
        // from red 0 g b → black (g and b are 0 for red).
        assert_eq!(
            parse_relative_color("rgb", "from red 0 g b"),
            Some(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 255
            })
        );
        // identity: from red r g b → red.
        close(parse_relative_color("rgb", "from red r g b"), 255, 0, 0);
    }

    #[test]
    fn relative_oklch_darkens() {
        // Halving OKLCh lightness darkens red but keeps it reddish.
        let c = parse_relative_color("oklch", "from red calc(l * 0.5) c h").unwrap();
        assert!(c.r < 200 && c.r > c.g && c.r > c.b, "got {c:?}");
    }

    #[test]
    fn light_dark_picks_first() {
        assert_eq!(parse_light_dark("red, blue"), named("red"));
        assert_eq!(parse_light_dark("only-one"), None);
    }

    #[test]
    fn modern_color_alpha_and_via_parse_color() {
        // `/ alpha` and dispatch through parse_color (verbatim token).
        let c = parse_modern_color(ModernSpace::Oklch, "1 0 0 / 0.5").unwrap();
        assert_eq!(c.a, 128);
        close(parse_color("oklch(1 0 0)"), 255, 255, 255);
    }

    // E44-M1
    #[test]
    fn named_full_set_and_basics_unchanged() {
        let rgb = |o: Option<Rgba>| o.map(|c| (c.r, c.g, c.b));
        // New CSS named colors resolve to their exact RGB.
        assert_eq!(rgb(named("rebeccapurple")), Some((102, 51, 153)));
        assert_eq!(rgb(named("tomato")), Some((255, 99, 71)));
        assert_eq!(rgb(named("darkslategray")), Some((47, 79, 79)));
        assert_eq!(rgb(named("cornflowerblue")), Some((100, 149, 237)));
        // Existing basics are byte-identical (note: named green is 0,128,0).
        assert_eq!(rgb(named("red")), Some((255, 0, 0)));
        assert_eq!(rgb(named("green")), Some((0, 128, 0)));
        assert_eq!(rgb(named("blue")), Some((0, 0, 255)));
        // gray/grey aliases agree; unknown keyword is None.
        assert_eq!(named("gray"), named("grey"));
        assert_eq!(named("notacolor"), None);
    }

    // E44-M1: the lookup table must stay sorted for the binary search.
    #[test]
    fn named_colors_table_is_sorted() {
        assert!(NAMED_COLORS.windows(2).all(|w| w[0].0 < w[1].0));
    }

    // E44-M1: case-insensitive entry through parse_color (caller lowercases).
    #[test]
    fn named_via_parse_color_case_insensitive() {
        let rgb = |o: Option<Rgba>| o.map(|c| (c.r, c.g, c.b));
        assert_eq!(rgb(parse_color("ReBeCcaPurple")), Some((102, 51, 153)));
        assert_eq!(rgb(parse_color("  TOMATO ")), Some((255, 99, 71)));
    }
}
