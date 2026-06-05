//! Rasterization (M5 §4): a white canvas, fill-rects via tiny-skia, and manual
//! src-over blitting of fontdue glyph coverage masks.

use tiny_skia::{Color, Paint, Pixmap, Rect as SkRect, Transform};

use starfish_style::Rgba;

use crate::display::PaintCmd;
use crate::font::{FontDb, GlyphBitmap};
use crate::image_store::ImageStore;

/// Paint the display list onto a fresh `width × height` white pixmap.
pub fn rasterize(
    cmds: &[PaintCmd],
    width: u32,
    height: u32,
    fonts: &FontDb,
    images: &ImageStore,
) -> Pixmap {
    // Dimensions are clamped to a sane range by callers, so this normally
    // succeeds; fall back to a 1×1 pixmap rather than panicking on bad input.
    let mut pixmap = Pixmap::new(width, height)
        .or_else(|| Pixmap::new(1, 1))
        .expect("1x1 pixmap always allocatable");
    pixmap.fill(Color::WHITE);

    for cmd in cmds {
        match cmd {
            PaintCmd::FillRect { rect, color } => fill_rect(&mut pixmap, rect, *color),
            PaintCmd::GlyphRun { .. } => draw_glyph_run(&mut pixmap, cmd, fonts),
            PaintCmd::ImageBlit { dest, src } => blit_image(&mut pixmap, dest, src, images),
        }
    }
    pixmap
}

fn fill_rect(pixmap: &mut Pixmap, rect: &starfish_layout::Rect, color: Rgba) {
    let mut paint = Paint {
        anti_alias: false,
        ..Default::default()
    };
    paint.set_color_rgba8(color.r, color.g, color.b, color.a);
    if let Some(r) = SkRect::from_xywh(rect.x, rect.y, rect.width.max(0.0), rect.height.max(0.0)) {
        pixmap.fill_rect(r, &paint, Transform::identity(), None);
    }
}

fn draw_glyph_run(pixmap: &mut Pixmap, cmd: &PaintCmd, fonts: &FontDb) {
    let PaintCmd::GlyphRun { origin, text, font_size, weight, color, ascent } = cmd else {
        return;
    };
    let baseline = origin.1 + ascent;
    let mut pen_x = origin.0;
    for ch in text.chars() {
        let g = fonts.rasterize_glyph(ch, *font_size, *weight);
        if g.width > 0 && g.height > 0 {
            let gx = (pen_x + g.left as f32).round() as i32;
            let gy = (baseline - g.top as f32).round() as i32;
            blit_coverage(pixmap, &g, gx, gy, *color);
        }
        pen_x += g.advance;
    }
}

/// Nearest-neighbour scale the decoded RGBA into `dest`, src-over into the
/// pixmap, clipped to bounds. Missing image (broken) → no-op.
fn blit_image(pixmap: &mut Pixmap, dest: &starfish_layout::Rect, src: &str, images: &ImageStore) {
    let Some(img) = images.peek(src) else { return };
    if img.width == 0 || img.height == 0 {
        return;
    }
    let dx0 = dest.x.round() as i32;
    let dy0 = dest.y.round() as i32;
    let dw = dest.width.round().max(0.0) as i32;
    let dh = dest.height.round().max(0.0) as i32;
    if dw == 0 || dh == 0 {
        return;
    }
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    // Clamp the iteration to the visible pixmap region so a huge `dw`/`dh`
    // (e.g. an upscaled-to-1e8 image) doesn't spin through trillions of
    // offscreen pixels. The scale ratio still uses the full `dw`/`dh` below so
    // the visible portion samples the source correctly.
    let x_start = dx0.max(0);
    let x_end = (dx0 + dw).min(pw);
    let y_start = dy0.max(0);
    let y_end = (dy0 + dh).min(ph);
    if x_start >= x_end || y_start >= y_end {
        return; // fully offscreen
    }
    let buf = pixmap.data_mut();
    for py in y_start..y_end {
        let ry = py - dy0;
        let sy = (ry as u32 * img.height / dh as u32).min(img.height - 1);
        for px in x_start..x_end {
            let rx = px - dx0;
            let sx = (rx as u32 * img.width / dw as u32).min(img.width - 1);
            let si = ((sy * img.width + sx) * 4) as usize;
            let (r, g, b, a) = (img.rgba[si], img.rgba[si + 1], img.rgba[si + 2], img.rgba[si + 3]);
            if a == 0 {
                continue;
            }
            let idx = ((py * pw + px) as usize) * 4;
            src_over_pixel(buf, idx, Rgba { r, g, b, a }, a);
        }
    }
}

/// Per-pixel src-over of `color` weighted by glyph coverage, clipped to bounds.
fn blit_coverage(pixmap: &mut Pixmap, g: &GlyphBitmap, gx: i32, gy: i32, color: Rgba) {
    let pw = pixmap.width() as i32;
    let ph = pixmap.height() as i32;
    let buf = pixmap.data_mut();
    for row in 0..g.height as i32 {
        let py = gy + row;
        if py < 0 || py >= ph {
            continue;
        }
        for col in 0..g.width as i32 {
            let px = gx + col;
            if px < 0 || px >= pw {
                continue;
            }
            let cov = g.coverage[row as usize * g.width + col as usize];
            if cov == 0 {
                continue;
            }
            let a = (cov as u32 * color.a as u32 / 255) as u8;
            if a == 0 {
                continue;
            }
            let idx = ((py * pw + px) as usize) * 4;
            src_over_pixel(buf, idx, color, a);
        }
    }
}

/// Composite a straight-color source (premultiplied by `a`) over the
/// premultiplied destination pixel at `idx`: `dst = src*a + dst*(1-a)`.
fn src_over_pixel(buf: &mut [u8], idx: usize, color: Rgba, a: u8) {
    let inv = 255 - a as u32;
    // source premultiplied by its alpha:
    let sr = color.r as u32 * a as u32 / 255;
    let sg = color.g as u32 * a as u32 / 255;
    let sb = color.b as u32 * a as u32 / 255;
    buf[idx] = (sr + buf[idx] as u32 * inv / 255) as u8;
    buf[idx + 1] = (sg + buf[idx + 1] as u32 * inv / 255) as u8;
    buf[idx + 2] = (sb + buf[idx + 2] as u32 * inv / 255) as u8;
    buf[idx + 3] = (a as u32 + buf[idx + 3] as u32 * inv / 255) as u8;
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_layout::Rect;

    #[test]
    fn blend_black_over_white() {
        // 1x1 white pixmap, blit full-coverage black → black.
        let mut buf = vec![255u8, 255, 255, 255];
        src_over_pixel(&mut buf, 0, Rgba { r: 0, g: 0, b: 0, a: 255 }, 255);
        assert_eq!(buf, vec![0, 0, 0, 255]);
    }

    #[test]
    fn blend_half_coverage() {
        // half coverage of red over white → ~ (255,128,128) premultiplied.
        let mut buf = vec![255u8, 255, 255, 255];
        src_over_pixel(&mut buf, 0, Rgba { r: 255, g: 0, b: 0, a: 255 }, 128);
        assert_eq!(buf[0], 255); // red stays full
        assert!(buf[1] > 120 && buf[1] < 135);
        assert!(buf[2] > 120 && buf[2] < 135);
        assert_eq!(buf[3], 255);
    }

    #[test]
    fn blit_downscale_samples_correct_color() {
        // 4×4 source (left half red, right half blue) downscaled to a 2×2 dest.
        // Nearest-neighbour: dest col 0 → src col 0 (red), dest col 1 → src col 2
        // (blue).
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starfish-blit-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                let px = if x < 2 {
                    image::Rgba([255, 0, 0, 255])
                } else {
                    image::Rgba([0, 0, 255, 255])
                };
                img.put_pixel(x, y, px);
            }
        }
        img.save(dir.join("q.png")).unwrap();

        let mut images = ImageStore::new(&dir);
        images.get("q.png").expect("decoded 4x4");

        let mut pm = Pixmap::new(2, 2).unwrap();
        pm.fill(Color::WHITE);
        blit_image(
            &mut pm,
            &Rect { x: 0.0, y: 0.0, width: 2.0, height: 2.0 },
            "q.png",
            &images,
        );
        let p0 = pm.pixel(0, 0).unwrap();
        let p1 = pm.pixel(1, 0).unwrap();
        assert_eq!((p0.red(), p0.green(), p0.blue()), (255, 0, 0)); // red half
        assert_eq!((p1.red(), p1.green(), p1.blue()), (0, 0, 255)); // blue half
    }

    #[test]
    fn fill_rect_paints_red() {
        let fonts = FontDb::load().unwrap();
        let cmds = vec![PaintCmd::FillRect {
            rect: Rect { x: 0.0, y: 0.0, width: 4.0, height: 4.0 },
            color: Rgba { r: 255, g: 0, b: 0, a: 255 },
        }];
        let pm = rasterize(&cmds, 4, 4, &fonts, &ImageStore::default());
        let px = pm.pixel(1, 1).unwrap();
        assert_eq!((px.red(), px.green(), px.blue()), (255, 0, 0));
    }
}
