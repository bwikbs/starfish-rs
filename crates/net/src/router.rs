//! `RouterLoader` — dispatch a fetch by URL scheme to the right concrete loader.
//!
//! Holds one `HttpLoader` (pooled agent) and one `LocalLoader`, so a single
//! `&dyn ResourceLoader` passed to `render_document` transparently handles a
//! page mixing `http(s)://` and `file://` resources. Routing is per-resolved
//! URL, so a remote page's relative CSS/images naturally route back through HTTP.

use std::time::Duration;

use url::Url;

use crate::data::DataLoader;
use crate::http::HttpLoader;
use crate::loader::{LoadError, LocalLoader, Resource, ResourceLoader};

/// Scheme-dispatching composite loader.
pub struct RouterLoader {
    local: LocalLoader,
    http: HttpLoader,
    data: DataLoader,
}

impl RouterLoader {
    /// Default HTTP timeout (30 s).
    pub fn new() -> Self {
        RouterLoader { local: LocalLoader, http: HttpLoader::new(), data: DataLoader }
    }

    /// With a custom HTTP timeout (CLI `--timeout`).
    pub fn with_timeout(timeout: Duration) -> Self {
        RouterLoader {
            local: LocalLoader,
            http: HttpLoader::with_timeout(timeout),
            data: DataLoader,
        }
    }
}

impl Default for RouterLoader {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceLoader for RouterLoader {
    fn fetch(&self, url: &Url) -> Result<Resource, LoadError> {
        match url.scheme() {
            "file" => self.local.fetch(url),
            "http" | "https" => self.http.fetch(url),
            "data" => self.data.fetch(url),
            other => Err(LoadError::UnsupportedScheme(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_scheme_is_rejected() {
        let r = RouterLoader::new();
        match r.fetch(&Url::parse("ftp://h/x").unwrap()) {
            Err(LoadError::UnsupportedScheme(s)) => assert_eq!(s, "ftp"),
            other => panic!("expected UnsupportedScheme, got {other:?}"),
        }
    }

    #[test]
    fn file_scheme_routes_to_local() {
        let dir = std::env::temp_dir().join(format!("starfish-router-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.txt");
        std::fs::write(&path, b"local-bytes").unwrap();
        let url = Url::from_file_path(&path).unwrap();
        let res = RouterLoader::new().fetch(&url).unwrap();
        assert_eq!(res.bytes, b"local-bytes");
    }

    #[test]
    fn data_scheme_routes_to_data() {
        let res = RouterLoader::new().fetch(&Url::parse("data:,hi").unwrap()).unwrap();
        assert_eq!(res.bytes, b"hi");
    }

    #[test]
    fn http_scheme_routes_to_http() {
        // A loopback fixture proves http URLs reach the HttpLoader.
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let handle = std::thread::spawn(move || {
            if let Ok(req) = server.recv() {
                let resp = tiny_http::Response::from_data(b"http-bytes".to_vec());
                let _ = req.respond(resp);
            }
        });
        let url = Url::parse(&format!("http://127.0.0.1:{port}/x")).unwrap();
        let res = RouterLoader::new().fetch(&url).unwrap();
        assert_eq!(res.bytes, b"http-bytes");
        let _ = handle.join();
    }
}
