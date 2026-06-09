//! `HttpLoader` — a blocking HTTP(S) GET behind the `ResourceLoader` trait.
//!
//! Holds one configured `ureq::Agent` (rustls TLS, redirect/timeout policy, our
//! User-Agent) so the document fetch and every linked CSS/image reuse one
//! client. Never panics — every failure is a `LoadError` value.

use std::time::Duration;

use ureq::ResponseExt;
use url::Url;

use crate::loader::{LoadError, Resource, ResourceLoader};

/// User-Agent advertised on every request.
const USER_AGENT: &str = concat!("starfish-rs/", env!("CARGO_PKG_VERSION"));

/// Redirect chain cap (handles `http→https`/canonical-host without looping).
const MAX_REDIRECTS: u32 = 10;

/// Hard cap on response body bytes read into memory (anti-OOM).
const MAX_BODY_BYTES: usize = 10 * 1024 * 1024; // 10 MiB

/// Default overall connect/read timeout.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Blocking HTTP(S) loader.
pub struct HttpLoader {
    agent: ureq::Agent,
    /// Hard cap on response body bytes read into memory.
    max_body_bytes: usize,
}

impl HttpLoader {
    /// Build with defaults: 10 MiB body cap, 30 s timeout, ≤10 redirects.
    pub fn new() -> Self {
        Self::with_timeout(DEFAULT_TIMEOUT)
    }

    /// Build with a caller-chosen overall timeout (CLI `--timeout`).
    pub fn with_timeout(timeout: Duration) -> Self {
        HttpLoader {
            agent: build_agent(timeout),
            max_body_bytes: MAX_BODY_BYTES,
        }
    }

    /// Build with a custom body cap (test-only knob for the body-cap path).
    #[cfg(test)]
    fn with_max_body(max_body_bytes: usize) -> Self {
        HttpLoader {
            agent: build_agent(DEFAULT_TIMEOUT),
            max_body_bytes,
        }
    }
}

impl Default for HttpLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// Construct the configured agent. 4xx/5xx come back as `Ok(resp)`
/// (`http_status_as_error(false)`); we map status ourselves in `fetch`.
fn build_agent(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(timeout))
        .timeout_recv_response(Some(timeout))
        .timeout_recv_body(Some(timeout))
        .user_agent(USER_AGENT)
        .max_redirects(MAX_REDIRECTS)
        .http_status_as_error(false)
        .build()
        .into()
}

impl ResourceLoader for HttpLoader {
    fn fetch(&self, url: &Url) -> Result<Resource, LoadError> {
        // GET. ureq follows redirects internally and returns the final response;
        // 4xx/5xx are Ok(resp) (mapped below). Transport failures are Err.
        let mut resp = match self.agent.get(url.as_str()).call() {
            Ok(resp) => resp,
            Err(e) => return Err(map_transport_error(e, url)),
        };

        // Status gate: non-2xx → LoadError::Http.
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            return Err(LoadError::Http {
                status,
                url: url.clone(),
            });
        }

        // Content-Type header → Resource.content_type (raw value; absent → None).
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // Final (post-redirect) URL: ureq exposes it on the response via
        // ResponseExt. Re-parse its string form into a url::Url; if that fails,
        // fall back to the requested url so relative sub-resources still resolve.
        let final_url = Url::parse(&resp.get_uri().to_string()).unwrap_or_else(|_| url.clone());

        // Read the body with a hard byte cap (anti-OOM).
        let bytes = resp
            .body_mut()
            .with_config()
            .limit(self.max_body_bytes as u64)
            .read_to_vec()
            .map_err(|e| map_body_error(e, url))?;

        Ok(Resource {
            bytes,
            content_type,
            final_url: Some(final_url),
        })
    }
}

/// Map a ureq transport error to a `LoadError`. Timeouts → `Timeout`;
/// everything else (DNS, connect refused, TLS, too-many-redirects, I/O) →
/// `Network` with the cause.
fn map_transport_error(e: ureq::Error, url: &Url) -> LoadError {
    match e {
        ureq::Error::Timeout(_) => LoadError::Timeout(url.clone()),
        other => LoadError::Network(format!("{other}")),
    }
}

/// Map a body-read error. A body over the cap surfaces as `Error::BodyExceedsLimit`
/// from ureq; report it (and any other read error) as `Network`.
fn map_body_error(e: ureq::Error, url: &Url) -> LoadError {
    match e {
        ureq::Error::Timeout(_) => LoadError::Timeout(url.clone()),
        ureq::Error::BodyExceedsLimit(cap) => {
            LoadError::Network(format!("response body exceeds {cap}-byte cap"))
        }
        other => LoadError::Network(format!("{other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// A canned response a fixture route returns.
    enum Canned {
        /// (status, content_type, body bytes)
        Ok(u16, &'static str, Vec<u8>),
        /// 302 to the given path.
        Redirect(&'static str),
        /// Sleep this long before replying 200 (timeout testing).
        Slow(Duration),
    }

    /// Guard that unblocks the fixture server's `recv` loop and joins its thread
    /// on drop.
    struct ServerGuard {
        server: Arc<tiny_http::Server>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl Drop for ServerGuard {
        fn drop(&mut self) {
            self.server.unblock(); // make the blocking recv() return → thread exits
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
        }
    }

    /// Spawn a tiny_http server on an ephemeral loopback port. `routes` maps a
    /// request path to a `Canned` response. Returns `(base_url, guard)`.
    fn serve(routes: impl Fn(&str) -> Canned + Send + 'static) -> (String, ServerGuard) {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let port = server.server_addr().to_ip().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");

        let srv = server.clone();
        let handle = thread::spawn(move || {
            // Blocking accept loop; `ServerGuard::drop` calls `unblock()` to end it.
            while let Ok(request) = srv.recv() {
                let path = request.url().to_string();
                match routes(&path) {
                    Canned::Ok(status, ct, body) => {
                        let resp = tiny_http::Response::from_data(body)
                            .with_status_code(status)
                            .with_header(
                                tiny_http::Header::from_bytes(&b"Content-Type"[..], ct.as_bytes())
                                    .unwrap(),
                            );
                        let _ = request.respond(resp);
                    }
                    Canned::Redirect(loc) => {
                        let resp = tiny_http::Response::empty(302).with_header(
                            tiny_http::Header::from_bytes(&b"Location"[..], loc.as_bytes())
                                .unwrap(),
                        );
                        let _ = request.respond(resp);
                    }
                    Canned::Slow(d) => {
                        thread::sleep(d);
                        let _ = request.respond(tiny_http::Response::from_string("late"));
                    }
                }
            }
        });

        (
            base,
            ServerGuard {
                server,
                handle: Some(handle),
            },
        )
    }

    fn url(base: &str, path: &str) -> Url {
        Url::parse(&format!("{base}{path}")).unwrap()
    }

    #[test]
    fn get_200_text_css_captures_bytes_and_content_type() {
        let (base, _g) =
            serve(|_path| Canned::Ok(200, "text/css; charset=utf-8", b"p{color:red}".to_vec()));
        let res = HttpLoader::new().fetch(&url(&base, "/style.css")).unwrap();
        assert_eq!(res.bytes, b"p{color:red}");
        assert_eq!(res.content_type.as_deref(), Some("text/css; charset=utf-8"));
    }

    #[test]
    fn get_binary_body_round_trips() {
        // Bytes that are NOT valid UTF-8 (PNG magic + a 0xFF run): proves the
        // loader carries arbitrary binary without mangling.
        let bin: Vec<u8> = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0xFF, 0xFE, 0x00,
        ];
        let bin2 = bin.clone();
        let (base, _g) = serve(move |_path| Canned::Ok(200, "image/png", bin2.clone()));
        let res = HttpLoader::new().fetch(&url(&base, "/px.png")).unwrap();
        assert_eq!(res.bytes, bin);
        assert_eq!(res.content_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn redirect_302_is_followed() {
        let (base, _g) = serve(|path| match path {
            "/a" => Canned::Redirect("/b"),
            _ => Canned::Ok(200, "text/plain", b"ok".to_vec()),
        });
        let res = HttpLoader::new().fetch(&url(&base, "/a")).unwrap();
        assert_eq!(res.bytes, b"ok");
    }

    #[test]
    fn redirect_final_url_resolves_relative_subresource() {
        // `/` 302-redirects to `/sub/page.html`; the page links a RELATIVE
        // `theme.css` served at `/sub/theme.css`. The fetched document's
        // final_url must be `/sub/page.html`, and resolving `theme.css` against
        // it must give `/sub/theme.css` (not the original-root `/theme.css`).
        let (base, _g) = serve(|path| match path {
            "/" => Canned::Redirect("/sub/page.html"),
            "/sub/page.html" => Canned::Ok(
                200,
                "text/html",
                b"<link rel='stylesheet' href='theme.css'>".to_vec(),
            ),
            _ => Canned::Ok(404, "text/plain", b"nope".to_vec()),
        });
        let res = HttpLoader::new().fetch(&url(&base, "/")).unwrap();

        let final_url = res.final_url.expect("http loader sets final_url");
        assert_eq!(final_url, url(&base, "/sub/page.html"));

        // Relative href resolves against the post-redirect base, not the input.
        let css = final_url.join("theme.css").unwrap();
        assert_eq!(css, url(&base, "/sub/theme.css"));
    }

    #[test]
    fn status_404_is_http_error() {
        let (base, _g) = serve(|_| Canned::Ok(404, "text/plain", b"nope".to_vec()));
        match HttpLoader::new().fetch(&url(&base, "/missing")) {
            Err(LoadError::Http { status, .. }) => assert_eq!(status, 404),
            other => panic!("expected Http 404, got {other:?}"),
        }
    }

    #[test]
    fn status_500_is_http_error() {
        let (base, _g) = serve(|_| Canned::Ok(500, "text/plain", b"boom".to_vec()));
        match HttpLoader::new().fetch(&url(&base, "/err")) {
            Err(LoadError::Http { status, .. }) => assert_eq!(status, 500),
            other => panic!("expected Http 500, got {other:?}"),
        }
    }

    #[test]
    fn body_over_cap_is_network_error_no_oom() {
        // Serve 4 KiB; cap at 1 KiB → over-cap → Network error, no OOM.
        let (base, _g) = serve(|_| Canned::Ok(200, "application/octet-stream", vec![b'x'; 4096]));
        match HttpLoader::with_max_body(1024).fetch(&url(&base, "/big")) {
            Err(LoadError::Network(m)) => assert!(m.contains("cap"), "msg: {m}"),
            other => panic!("expected Network over-cap, got {other:?}"),
        }
    }

    #[test]
    fn timeout_is_timeout_error() {
        let (base, _g) = serve(|_| Canned::Slow(Duration::from_secs(2)));
        let loader = HttpLoader::with_timeout(Duration::from_millis(100));
        match loader.fetch(&url(&base, "/slow")) {
            Err(LoadError::Timeout(_)) => {}
            other => panic!("expected Timeout, got {other:?}"),
        }
    }

    #[test]
    fn connection_refused_is_network_error_no_panic() {
        // Nothing listens on port 1 of loopback.
        let u = Url::parse("http://127.0.0.1:1/x").unwrap();
        match HttpLoader::new().fetch(&u) {
            Err(LoadError::Network(_)) => {}
            other => panic!("expected Network, got {other:?}"),
        }
    }
}
