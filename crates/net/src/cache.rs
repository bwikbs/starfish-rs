//! `CachingLoader<L>` — an in-memory, per-render fetch cache wrapping any
//! `ResourceLoader`.
//!
//! The first `fetch` of a URL hits the inner loader; the outcome (success OR
//! error) is memoized and returned to every later `fetch` of the same URL, so a
//! repeated document/CSS/image URL is fetched once per render.
//!
//! Scope & thread-safety: built fresh per render and dropped after (no
//! cross-render or on-disk cache). The whole fetch-then-render pipeline is
//! single-threaded, so `RefCell<HashMap>` + `Rc` are sound and lock-free — we
//! deliberately avoid `Mutex`/`Arc` (no concurrency to protect against).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use url::Url;

use crate::loader::{LoadError, Resource, ResourceLoader};

/// Wraps any `ResourceLoader` and dedupes `fetch` by URL.
pub struct CachingLoader<L: ResourceLoader> {
    inner: L,
    cache: RefCell<HashMap<Url, Rc<Result<Resource, LoadError>>>>,
}

impl<L: ResourceLoader> CachingLoader<L> {
    pub fn new(inner: L) -> Self {
        CachingLoader {
            inner,
            cache: RefCell::new(HashMap::new()),
        }
    }
}

impl<L: ResourceLoader> ResourceLoader for CachingLoader<L> {
    fn fetch(&self, url: &Url) -> Result<Resource, LoadError> {
        if let Some(slot) = self.cache.borrow().get(url).cloned() {
            // Hit: a repeat fetch. `Ok` clones the resource; an error is flattened
            // to `Network` (LoadError is not Clone), which is fine for repeats.
            return rematerialize(&slot);
        }
        // Miss: fetch once and return the inner loader's ORIGINAL result (its
        // exact error variant preserved for the CLI's friendly-message mapping).
        // Memoize a cacheable copy alongside it so a later hit can re-materialize
        // the outcome without re-fetching.
        let result = self.inner.fetch(url);
        let slot = Rc::new(match &result {
            Ok(res) => Ok(res.clone()),
            Err(e) => Err(LoadError::Network(format!("{e}"))),
        });
        self.cache.borrow_mut().insert(url.clone(), slot);
        result
    }
}

/// Produce an owned `Result` from a cached slot. `Ok` clones the `Resource`.
/// Errors are cached too (so a dead URL isn't re-fetched on every reference);
/// since `LoadError` is not `Clone`, a cached error is re-materialized as a
/// flattened `LoadError::Network(Display-of-original)`. This is harmless: every
/// sub-resource caller treats any `Err` identically (skip), and the CLI only
/// inspects the variant on the *first* (top-level) fetch, which is a cache miss
/// returning the inner loader's original, un-flattened error.
fn rematerialize(slot: &Rc<Result<Resource, LoadError>>) -> Result<Resource, LoadError> {
    match &**slot {
        Ok(res) => Ok(res.clone()),
        Err(e) => Err(LoadError::Network(format!("{e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    /// A loader that counts its calls and returns a canned outcome.
    struct Counting {
        hits: Cell<u32>,
        ok: bool,
    }

    impl ResourceLoader for Counting {
        fn fetch(&self, url: &Url) -> Result<Resource, LoadError> {
            self.hits.set(self.hits.get() + 1);
            if self.ok {
                Ok(Resource {
                    bytes: b"body".to_vec(),
                    content_type: None,
                    final_url: Some(url.clone()),
                })
            } else {
                Err(LoadError::Http {
                    status: 404,
                    url: url.clone(),
                })
            }
        }
    }

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn success_is_fetched_once_per_url() {
        let cache = CachingLoader::new(Counting {
            hits: Cell::new(0),
            ok: true,
        });
        let a = u("http://h/a");
        let r1 = cache.fetch(&a).unwrap();
        let r2 = cache.fetch(&a).unwrap();
        assert_eq!(r1.bytes, b"body");
        assert_eq!(r2.bytes, b"body"); // owned copy on the hit
        assert_eq!(cache.inner.hits.get(), 1, "same URL fetched once");

        // A different URL bumps the inner loader.
        cache.fetch(&u("http://h/b")).unwrap();
        assert_eq!(cache.inner.hits.get(), 2);
    }

    #[test]
    fn error_is_cached_and_not_refetched() {
        let cache = CachingLoader::new(Counting {
            hits: Cell::new(0),
            ok: false,
        });
        let a = u("http://h/missing");
        assert!(cache.fetch(&a).is_err());
        assert!(cache.fetch(&a).is_err());
        assert_eq!(
            cache.inner.hits.get(),
            1,
            "failed URL fetched once (error cached)"
        );
    }

    /// A loader that counts its calls and always fails with a specific,
    /// non-`Network` error variant.
    struct CountingUnsupported {
        hits: Cell<u32>,
    }

    impl ResourceLoader for CountingUnsupported {
        fn fetch(&self, _url: &Url) -> Result<Resource, LoadError> {
            self.hits.set(self.hits.get() + 1);
            Err(LoadError::UnsupportedScheme("blob".to_string()))
        }
    }

    #[test]
    fn miss_preserves_original_error_variant_hit_flattens_to_network() {
        let cache = CachingLoader::new(CountingUnsupported { hits: Cell::new(0) });
        let a = u("http://h/x");

        // First fetch (cache miss): the inner loader's ORIGINAL variant is returned.
        match cache.fetch(&a) {
            Err(LoadError::UnsupportedScheme(s)) => assert_eq!(s, "blob"),
            other => panic!("expected original UnsupportedScheme on miss, got {other:?}"),
        }

        // Second fetch (cache hit): the cached error is flattened to Network.
        match cache.fetch(&a) {
            Err(LoadError::Network(_)) => {}
            other => panic!("expected flattened Network on hit, got {other:?}"),
        }

        assert_eq!(
            cache.inner.hits.get(),
            1,
            "inner loader hit once (error cached)"
        );
    }

    #[test]
    fn cached_ok_returns_independent_owned_bytes() {
        let cache = CachingLoader::new(Counting {
            hits: Cell::new(0),
            ok: true,
        });
        let a = u("http://h/a");
        let mut r1 = cache.fetch(&a).unwrap();
        r1.bytes.clear(); // mutate the first caller's copy
        let r2 = cache.fetch(&a).unwrap();
        assert_eq!(
            r2.bytes, b"body",
            "cached copy unaffected by the caller's mutation"
        );
    }
}
