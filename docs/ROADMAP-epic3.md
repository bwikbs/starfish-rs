# Roadmap — Epic 3: networking & resource loading

Turns the engine from "render an HTML string with local assets" into "render a page
identified by a URL, fetching its linked resources." Same per-milestone agent pipeline
(design → analysis → implementation → review → verification), each landing as its own
commit + push. Still no JavaScript.

Target: `starfish render <url|path> -o out.png` resolves and loads the page's external
CSS (`<link rel=stylesheet>`), images, etc., over `file://` and `http(s)://`.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E3-M1** | URL model + `ResourceLoader` trait + `file://`/local-path scheme; relative-URL resolution against a base; load `<link rel="stylesheet">` and merge author CSS in document order with `<style>`. Refactor image loading onto the loader. | new `net`, `paint`, `cli` | A page with a `<link>`-ed local stylesheet and a local `<img>` renders correctly via a URL/path (tested + visual) | ✅ |
| **E3-M2** | HTTP(S) fetching (a blocking pure-Rust client, e.g. `ureq`+rustls): fetch the HTML document, linked CSS, and remote images over `http(s)`; redirects, status/content-type, timeouts; CLI accepts a URL. | `net`, `cli` | Rendering a remote URL fetches and applies its CSS + images (tested with a local HTTP fixture server; graceful on errors) | ✅ |
| **E3-M3** | `data:` URLs (base64/utf8 for img + css), an in-memory resource cache (dedupe by URL), redirect/error robustness, content-type/charset basics. | `net` | `data:` image/CSS render; repeated URLs fetched once; malformed/404 resources degrade gracefully (tested) | ✅ |

**Epic 3 complete.** 350 workspace tests, clippy clean. The renderer fetches pages and their CSS/images over `file://`, `http(s)://`, and `data:`.

## Non-goals (deferred to later epics)

- JavaScript / `<script>` execution / events.
- Cookies, auth, caching headers (ETag/Cache-Control), HTTP/2, compression beyond what
  the client gives for free, service workers.
- `@import` CSS (note: could be folded into M1/M3 if cheap), `<base>` element (maybe M1),
  favicons, prefetch/preload, CORS, mixed-content policy, TLS cert pinning.
- Streaming/incremental parse while loading; everything is fetch-then-render.
