//! E3-M2 end-to-end: render a page fetched over HTTP, proving its linked CSS and
//! `<img>` are fetched over the same `RouterLoader` and applied. A loopback
//! `tiny_http` fixture serves the HTML, the stylesheet, and a PNG; we fetch the
//! document by `http://` URL, render it, and sample pixels.

use std::sync::Arc;
use std::thread;

use starfish_paint::{render_document, render_url, Pixmap, ResourceLoader, RouterLoader, Url};

/// Build a 2×2 PNG (TL red, TR green, BL blue, BR white).
fn make_png() -> Vec<u8> {
    let mut img = image::RgbaImage::new(2, 2);
    img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
    img.put_pixel(1, 0, image::Rgba([0, 255, 0, 255]));
    img.put_pixel(0, 1, image::Rgba([0, 0, 255, 255]));
    img.put_pixel(1, 1, image::Rgba([255, 255, 255, 255]));
    let mut buf = std::io::Cursor::new(Vec::new());
    img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
    buf.into_inner()
}

struct Guard {
    server: Arc<tiny_http::Server>,
    handle: Option<thread::JoinHandle<()>>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        self.server.unblock();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn px(pm: &Pixmap, x: u32, y: u32) -> (u8, u8, u8, u8) {
    let p = pm.pixel(x, y).expect("in bounds");
    (p.red(), p.green(), p.blue(), p.alpha())
}

/// Scan the pixmap region `[0,max_x) × [0,max_y)` for a pixel satisfying `pred`.
fn has_color_in(
    pm: &Pixmap,
    max_x: u32,
    max_y: u32,
    pred: impl Fn(u8, u8, u8) -> bool,
) -> bool {
    for y in 0..pm.height().min(max_y) {
        for x in 0..pm.width().min(max_x) {
            let (r, g, b, _) = px(pm, x, y);
            if pred(r, g, b) {
                return true;
            }
        }
    }
    false
}

#[test]
fn renders_remote_page_with_http_css_and_image() {
    let png = make_png();
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let port = server.server_addr().to_ip().unwrap().port();

    let srv = server.clone();
    let handle = thread::spawn(move || {
        while let Ok(request) = srv.recv() {
            let path = request.url().to_string();
            let (ct, body): (&str, Vec<u8>) = match path.as_str() {
                "/" => (
                    "text/html",
                    b"<html><head><link rel='stylesheet' href='theme.css'>\
                      <style>body{margin:0} img{display:block}</style></head>\
                      <body><p>hi</p><img src='px.png' width='4' height='4'></body></html>"
                        .to_vec(),
                ),
                "/theme.css" => ("text/css", b"p{color:blue}".to_vec()),
                "/px.png" => ("image/png", png.clone()),
                _ => ("text/plain", b"not found".to_vec()),
            };
            let resp = tiny_http::Response::from_data(body).with_header(
                tiny_http::Header::from_bytes(&b"Content-Type"[..], ct.as_bytes()).unwrap(),
            );
            let _ = request.respond(resp);
        }
    });
    let _guard = Guard { server, handle: Some(handle) };

    let base = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    let loader = RouterLoader::new();
    let html = String::from_utf8_lossy(&loader.fetch(&base).unwrap().bytes).into_owned();
    let pm = render_document(&html, &base, 200.0, &loader);

    // The CSS fetched over HTTP was applied → blue text in the top text band
    // (above the image), and no red text (which would mean CSS was missing).
    assert!(
        has_color_in(&pm, 200, 40, |r, g, b| b > 120 && r < 80 && g < 80),
        "expected blue text from http css"
    );
    // The PNG fetched over HTTP decoded → its red top-left quadrant is present
    // somewhere in the (taller) page below the text.
    assert!(
        has_color_in(&pm, pm.width(), pm.height(), |r, g, b| r > 200 && g < 80 && b < 80),
        "expected red pixel from http png"
    );
}

#[test]
fn renders_redirected_page_resolving_relative_css_against_final_url() {
    // `/` 302-redirects to `/sub/page.html`, which links a RELATIVE `theme.css`
    // served only at `/sub/theme.css` (NOT at the original-root `/theme.css`).
    // render_url must use the post-redirect URL as the base, otherwise the CSS
    // is fetched from the wrong path and the page renders without it.
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let port = server.server_addr().to_ip().unwrap().port();

    let srv = server.clone();
    let handle = thread::spawn(move || {
        while let Ok(request) = srv.recv() {
            let path = request.url().to_string();
            match path.as_str() {
                "/" => {
                    let resp = tiny_http::Response::empty(302).with_header(
                        tiny_http::Header::from_bytes(&b"Location"[..], &b"/sub/page.html"[..])
                            .unwrap(),
                    );
                    let _ = request.respond(resp);
                }
                _ => {
                    let (ct, body): (&str, Vec<u8>) = match path.as_str() {
                        "/sub/page.html" => (
                            "text/html",
                            b"<html><head><link rel='stylesheet' href='theme.css'>\
                              <style>body{margin:0}</style></head>\
                              <body><p>hi</p></body></html>"
                                .to_vec(),
                        ),
                        // Only the post-redirect-relative path serves blue CSS.
                        "/sub/theme.css" => ("text/css", b"p{color:blue}".to_vec()),
                        // The wrong (original-root) path serves red CSS, so a
                        // base-resolution bug would show red instead of blue.
                        "/theme.css" => ("text/css", b"p{color:red}".to_vec()),
                        _ => ("text/plain", b"not found".to_vec()),
                    };
                    let resp = tiny_http::Response::from_data(body).with_header(
                        tiny_http::Header::from_bytes(&b"Content-Type"[..], ct.as_bytes())
                            .unwrap(),
                    );
                    let _ = request.respond(resp);
                }
            }
        }
    });
    let _guard = Guard { server, handle: Some(handle) };

    let input = Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap();
    let pm = render_url(&input, 200.0).expect("render redirected page");

    // Blue text proves the relative CSS resolved against `/sub/page.html`.
    assert!(
        has_color_in(&pm, 200, 40, |r, g, b| b > 120 && r < 80 && g < 80),
        "expected blue text from post-redirect relative css"
    );
    // No red text → we did NOT resolve `theme.css` against the original root.
    assert!(
        !has_color_in(&pm, 200, 40, |r, g, b| r > 120 && g < 80 && b < 80),
        "unexpected red text: relative css resolved against original url, not final url"
    );
}
