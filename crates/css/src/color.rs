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
