//! The `ResourceLoader` trait, its `LoadError` value type, and `LocalLoader`
//! (`file://` only in M1). Implementations never panic — every failure is a
//! `LoadError` value.

use std::path::PathBuf;

use url::Url;

/// A resource-load failure, carried as a value (callers decide whether to skip
/// or surface it). No variant ever results from a panic.
#[derive(Debug)]
pub enum LoadError {
    /// The resource does not exist (e.g. a `file://` path not found).
    NotFound(Url),
    /// An I/O error while reading (permissions, mid-read failure, cwd lookup).
    Io(std::io::Error),
    /// A scheme no loader handles yet (`http`/`https` in M1, `data:` in M1/M2).
    UnsupportedScheme(String),
    /// A `file://` URL that doesn't map to a local path (a host is present, …).
    BadPath(PathBuf),
    /// A relative `href` that failed to resolve against the base.
    BadUrl(url::ParseError),
    /// A non-2xx HTTP final status (after redirects).
    Http { status: u16, url: Url },
    /// The request exceeded the connect/read timeout.
    Timeout(Url),
    /// A transport-level failure (DNS, connect refused, TLS, redirect loop,
    /// over-size body) — the message carries the cause.
    Network(String),
    /// A malformed `data:` URL — no comma, bad base64, or over the size cap.
    BadDataUrl(Url),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::NotFound(u) => write!(f, "resource not found: {u}"),
            LoadError::Io(e) => write!(f, "i/o error: {e}"),
            LoadError::UnsupportedScheme(s) => {
                write!(f, "{s}:// URLs are not supported yet")
            }
            LoadError::BadPath(p) => write!(f, "not a local path: {}", p.display()),
            LoadError::BadUrl(e) => write!(f, "bad URL: {e}"),
            LoadError::Http { status, url } => write!(f, "HTTP {status} for {url}"),
            LoadError::Timeout(u) => write!(f, "request timed out: {u}"),
            LoadError::Network(m) => write!(f, "network error: {m}"),
            LoadError::BadDataUrl(u) => write!(f, "malformed data: URL: {u}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Io(e) => Some(e),
            LoadError::BadUrl(e) => Some(e),
            _ => None,
        }
    }
}

/// A fetched resource: the raw bytes plus an optional content type. M1 fills
/// `content_type` only when a loader knows it cheaply (`LocalLoader` leaves it
/// `None`). The field is carried now so M2's `HttpLoader` can populate it from
/// the `Content-Type` header without changing the trait.
///
/// `final_url` is the resource's location *after* any HTTP redirects (the
/// `HttpLoader` sets it from the final response URI; `LocalLoader` sets the
/// requested file URL). Callers use it as the base for resolving the document's
/// relative sub-resources, so a 302 from `/` → `/sub/page` resolves a relative
/// `theme.css` against `/sub/` rather than the original input URL.
///
/// `Clone` is derived (all fields are `Clone`) so the `CachingLoader` can hand
/// an owned copy of a memoized resource back on every cache hit.
#[derive(Debug, Clone)]
pub struct Resource {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
    pub final_url: Option<Url>,
}

/// Fetches the bytes of a resource named by an absolute `Url`. Implementations
/// MUST NOT panic — every failure is a `LoadError`. The `&self` receiver lets a
/// loader hold config/cache and be shared as `&dyn ResourceLoader`, so an
/// `HttpLoader` (M2) and a scheme-routing composite are drop-in additions.
pub trait ResourceLoader {
    fn fetch(&self, url: &Url) -> Result<Resource, LoadError>;
}

/// Reads `file://` URLs from the local filesystem. The only loader in M1.
pub struct LocalLoader;

impl ResourceLoader for LocalLoader {
    fn fetch(&self, url: &Url) -> Result<Resource, LoadError> {
        match url.scheme() {
            "file" => {
                let path = file_path_of(url)
                    .ok_or_else(|| LoadError::BadPath(PathBuf::from(url.path())))?;
                match std::fs::read(&path) {
                    Ok(bytes) => Ok(Resource {
                        bytes,
                        content_type: None,
                        final_url: Some(url.clone()),
                    }),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        Err(LoadError::NotFound(url.clone()))
                    }
                    Err(e) => Err(LoadError::Io(e)),
                }
            }
            other => Err(LoadError::UnsupportedScheme(other.to_string())),
        }
    }
}

/// The filesystem path a `file://` URL refers to, or `None` if `url` is not a
/// `file:` URL (or has a host / is otherwise not a local path). `to_file_path`
/// strips the scheme, percent-decodes, and rejects a non-empty host on Unix.
pub(crate) fn file_path_of(url: &Url) -> Option<PathBuf> {
    if url.scheme() != "file" {
        return None;
    }
    url.to_file_path().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn temp_dir() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("starfish-net-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn local_loader_reads_temp_file() {
        let dir = temp_dir();
        let path = dir.join("hello.txt");
        std::fs::write(&path, b"hi there").unwrap();
        let url = Url::from_file_path(&path).unwrap();

        let res = LocalLoader.fetch(&url).expect("read ok");
        assert_eq!(res.bytes, b"hi there");
        assert_eq!(res.content_type, None);
        // final_url is the requested file URL (no redirects for file://).
        assert_eq!(res.final_url.as_ref(), Some(&url));
    }

    #[test]
    fn local_loader_missing_file_is_not_found() {
        let dir = temp_dir();
        let url = Url::from_file_path(dir.join("nope.txt")).unwrap();
        match LocalLoader.fetch(&url) {
            Err(LoadError::NotFound(_)) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn local_loader_http_is_unsupported_scheme() {
        let url = Url::parse("http://example.com/x.css").unwrap();
        match LocalLoader.fetch(&url) {
            Err(LoadError::UnsupportedScheme(s)) => assert_eq!(s, "http"),
            other => panic!("expected UnsupportedScheme, got {other:?}"),
        }
    }
}
