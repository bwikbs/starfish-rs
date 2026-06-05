//! starfish-net (E3-M1) — URL model + `ResourceLoader` abstraction.
//!
//! A leaf utility crate: it owns the document/resource URL model (over the
//! `url` crate) and a bytes-in/bytes-out loader trait. No dependency on
//! dom/style/paint, so the render pipeline can depend on it without cycles.
//!
//! M1 ships only `LocalLoader` (`file://`). The trait is shaped so an
//! `HttpLoader` (E3-M2) and a scheme-routing composite slot in without any
//! trait change — every consumer takes `&dyn ResourceLoader`.

pub use url::Url;

mod http;
mod loader;
mod resolve;
mod router;

pub use http::HttpLoader;
pub use loader::{LoadError, LocalLoader, Resource, ResourceLoader};
pub use resolve::{base_url_from_input, file_url_from_path};
pub use router::RouterLoader;
