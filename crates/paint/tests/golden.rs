//! Golden pipeline test (M5 §9.6): render a fixed fixture and assert its
//! dimensions plus a handful of font-independent sampled pixels (background and
//! border). Avoids full-image PNG goldens, which break on any font update.

use starfish_paint::{render_html_cwd, Pixmap};

fn px(pm: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
    let p = pm.pixel(x, y).expect("in bounds");
    (p.red(), p.green(), p.blue(), p.alpha())
}

#[test]
fn golden_fixture_dimensions_and_pixels() {
    let html = include_str!("fixtures/page.html");
    let pm = render_html_cwd(html, 200.0);

    // Width is fixed by the viewport; height grows with content.
    assert_eq!(pm.width(), 200);
    // box: 60px content + 2*4px border = 68px, then the 20px-line paragraph.
    assert!(pm.height() >= 68, "height={}", pm.height());

    // Top border band of the box (y in 0..4) → blue.
    assert_eq!(px(&pm, 50, 1), (0, 0, 255, 255));
    // Inside the box (past the 4px border) → red.
    assert_eq!(px(&pm, 50, 30), (255, 0, 0, 255));
    // Far right of the box (box is 108px wide incl. border) → white.
    assert_eq!(px(&pm, 180, 30), (255, 255, 255, 255));

    // Text region: at least one non-white pixel just below the box.
    let mut found_text = false;
    'scan: for y in 68..pm.height().min(100) {
        for x in 0..pm.width().min(160) {
            if px(&pm, x, y).0 != 255 || px(&pm, x, y).1 != 255 || px(&pm, x, y).2 != 255 {
                found_text = true;
                break 'scan;
            }
        }
    }
    assert!(found_text, "expected text pixels below the box");
}
