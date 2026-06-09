//! `DataLoader` — decode `data:` URLs (RFC 2397) into in-memory bytes.
//!
//! Grammar handled: `data:[<mediatype>][;base64],<data>`. Everything before the
//! first comma is the *header* (media type + optional params + an optional
//! trailing `;base64`); everything after is the *payload*. A base64 payload is
//! whitespace-stripped then `STANDARD`-decoded; a plain-text payload is
//! percent-decoded to its bytes (`+` stays a literal plus — `data:` is RFC 2397,
//! not form-encoding). Stateless, never panics — every malformed input is a
//! `LoadError::BadDataUrl`.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use url::Url;

use crate::loader::{LoadError, Resource, ResourceLoader};

/// Hard cap on decoded `data:` payload size (anti-OOM, mirrors the HTTP body
/// cap). A base64 image inline can be large but not unbounded.
const MAX_DATA_BYTES: usize = 10 * 1024 * 1024; // 10 MiB

/// Decodes `data:` URLs (RFC 2397) into in-memory bytes.
pub struct DataLoader;

impl ResourceLoader for DataLoader {
    fn fetch(&self, url: &Url) -> Result<Resource, LoadError> {
        if url.scheme() != "data" {
            return Err(LoadError::UnsupportedScheme(url.scheme().to_string()));
        }
        // For a `data:` URL `url.path()` returns the raw "header,payload"
        // substring WITHOUT percent-decoding (verified, see design §1.3).
        let after = url.path();
        let (header, payload) = after
            .split_once(',')
            .ok_or_else(|| LoadError::BadDataUrl(url.clone()))?;

        // `;base64` flag (case-insensitive) + media type.
        let is_base64 = header
            .rsplit(';')
            .next()
            .is_some_and(|t| t.eq_ignore_ascii_case("base64"));
        let media = media_type(header);

        // Decode payload → bytes.
        let bytes = if is_base64 {
            // STANDARD rejects whitespace; data: base64 commonly wraps with
            // newlines, so strip ascii whitespace before decoding.
            let cleaned: String = payload
                .chars()
                .filter(|c| !c.is_ascii_whitespace())
                .collect();
            STANDARD
                .decode(&cleaned)
                .map_err(|_| LoadError::BadDataUrl(url.clone()))?
        } else {
            // Plain-text path: the payload bytes ARE percent-decoded text.
            percent_decode_bytes(payload)
        };

        // Size cap (anti-OOM).
        if bytes.len() > MAX_DATA_BYTES {
            return Err(LoadError::BadDataUrl(url.clone()));
        }

        Ok(Resource {
            bytes,
            content_type: media,
            final_url: Some(url.clone()), // data: is self-contained, no redirect
        })
    }
}

/// Media type = header up to the first `;` (or the whole header). Empty → `None`
/// (let the element type drive parsing). The `;base64` suffix and any params
/// (`charset=…`, `name=…`) are dropped.
fn media_type(header: &str) -> Option<String> {
    let mime = header.split(';').next().unwrap_or("").trim();
    if mime.is_empty() {
        None
    } else {
        Some(mime.to_string())
    }
}

/// Percent-decode a `data:` text payload into bytes. `%XX` (two hex digits)
/// becomes the byte; a malformed `%` sequence (`%`, `%G1`, trailing) is passed
/// through literally (browser leniency, no hard failure). `+` is a literal plus.
fn percent_decode_bytes(s: &str) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let (Some(h), Some(l)) = (
                bytes.get(i + 1).and_then(|b| hex_val(*b)),
                bytes.get(i + 2).and_then(|b| hex_val(*b)),
            ) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

/// Value of one ASCII hex digit, or `None`.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fetch(s: &str) -> Result<Resource, LoadError> {
        DataLoader.fetch(&Url::parse(s).unwrap())
    }

    #[test]
    fn base64_png_round_trips() {
        // Arbitrary binary (PNG magic + a 0xFF run): not valid UTF-8, proves the
        // base64 path carries raw bytes intact. (Decode-to-image is asserted in
        // the paint integration tests, which already depend on the image crate.)
        let png: Vec<u8> = vec![
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0xFF, 0xFE, 0x00,
        ];
        let b64 = STANDARD.encode(&png);
        let res = fetch(&format!("data:image/png;base64,{b64}")).unwrap();
        assert_eq!(res.bytes, png);
        assert_eq!(res.content_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn plain_text_css_percent_decoded() {
        let res = fetch("data:text/css,p%7Bcolor%3Ared%7D").unwrap();
        assert_eq!(res.bytes, b"p{color:red}");
        assert_eq!(res.content_type.as_deref(), Some("text/css"));
    }

    #[test]
    fn base64_text_css() {
        let b64 = STANDARD.encode("p{color:red}");
        let res = fetch(&format!("data:text/css;base64,{b64}")).unwrap();
        assert_eq!(res.bytes, b"p{color:red}");
        assert_eq!(res.content_type.as_deref(), Some("text/css"));
    }

    #[test]
    fn empty_media_type_default_none() {
        let res = fetch("data:,hello").unwrap();
        assert_eq!(res.bytes, b"hello");
        assert_eq!(res.content_type, None);
    }

    #[test]
    fn empty_media_type_base64() {
        let res = fetch("data:;base64,SGk=").unwrap();
        assert_eq!(res.bytes, b"Hi");
        assert_eq!(res.content_type, None);
    }

    #[test]
    fn percent_space_decoded() {
        let res = fetch("data:,a%20b").unwrap();
        assert_eq!(res.bytes, b"a b");
    }

    #[test]
    fn plus_is_literal() {
        let res = fetch("data:,a+b").unwrap();
        assert_eq!(res.bytes, b"a+b");
    }

    #[test]
    fn base64_with_embedded_newlines_decodes() {
        // Base64 of "Hello, World!" wrapped across lines (whitespace stripped).
        let b64 = STANDARD.encode("Hello, World!");
        let mid = b64.len() / 2;
        let wrapped = format!("{}\n{}", &b64[..mid], &b64[mid..]);
        let res = fetch(&format!("data:text/plain;base64,{wrapped}")).unwrap();
        assert_eq!(res.bytes, b"Hello, World!");
    }

    #[test]
    fn base64_case_insensitive_flag() {
        let b64 = STANDARD.encode("Hi");
        let res = fetch(&format!("data:text/plain;BASE64,{b64}")).unwrap();
        assert_eq!(res.bytes, b"Hi");
        assert_eq!(res.content_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn no_comma_is_bad_data_url() {
        match fetch("data:text/plain") {
            Err(LoadError::BadDataUrl(_)) => {}
            other => panic!("expected BadDataUrl, got {other:?}"),
        }
    }

    #[test]
    fn bad_base64_is_bad_data_url() {
        match fetch("data:image/png;base64,@@@notb64@@@") {
            Err(LoadError::BadDataUrl(_)) => {}
            other => panic!("expected BadDataUrl, got {other:?}"),
        }
    }

    #[test]
    fn over_cap_is_bad_data_url() {
        // Build a base64 payload whose decoded length exceeds MAX_DATA_BYTES.
        let big = vec![b'x'; MAX_DATA_BYTES + 1];
        let b64 = STANDARD.encode(&big);
        match fetch(&format!("data:application/octet-stream;base64,{b64}")) {
            Err(LoadError::BadDataUrl(_)) => {}
            other => panic!("expected BadDataUrl over-cap, got {other:?}"),
        }
    }

    #[test]
    fn final_url_is_the_data_url() {
        let url = Url::parse("data:,hi").unwrap();
        let res = DataLoader.fetch(&url).unwrap();
        assert_eq!(res.final_url.as_ref(), Some(&url));
    }
}
