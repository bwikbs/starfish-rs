//! Building a document base `Url` from a user-supplied input (a URL string or a
//! filesystem path). `url::Url` already does absolute parse, relative `join`,
//! scheme detection, and `file://` ↔ path conversion; these two free functions
//! cover the one operation it doesn't spell directly.

use std::path::Path;

use url::Url;

use crate::loader::LoadError;

/// Turn a user-supplied input (a URL string OR a filesystem path) into the
/// document's absolute base `Url`.
///
/// - If `input` parses as an absolute URL (`file://…`, `http://…`, …) it is
///   used as-is.
/// - Otherwise `input` is treated as a filesystem path and converted to a
///   `file://` URL: relative paths are first made absolute by joining the
///   current working directory.
pub fn base_url_from_input(input: &str) -> Result<Url, LoadError> {
    // `Url::parse` succeeds only for an absolute URL with a scheme; a bare path
    // like "page.html" or "./sub/p.html" fails here and falls through to path
    // handling.
    if let Ok(u) = Url::parse(input) {
        return Ok(u);
    }
    file_url_from_path(Path::new(input))
}

/// Convert a filesystem path to an absolute `file://` URL. Relative paths join
/// the current working directory. The path need not exist (resolution is
/// lexical; the loader reports a missing file at fetch time).
pub fn file_url_from_path(path: &Path) -> Result<Url, LoadError> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(LoadError::Io)?.join(path)
    };
    Url::from_file_path(&abs).map_err(|()| LoadError::BadPath(abs))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_path_becomes_cwd_joined_file_url() {
        let url = base_url_from_input("page.html").unwrap();
        assert_eq!(url.scheme(), "file");
        assert!(url.path().ends_with("/page.html"), "path: {}", url.path());
        let cwd = std::env::current_dir().unwrap();
        let expected = Url::from_file_path(cwd.join("page.html")).unwrap();
        assert_eq!(url, expected);
    }

    #[test]
    fn absolute_path_becomes_file_url() {
        let url = base_url_from_input("/abs/page.html").unwrap();
        assert_eq!(url.scheme(), "file");
        assert_eq!(url.path(), "/abs/page.html");
    }

    #[test]
    fn file_url_passes_through_unchanged() {
        let url = base_url_from_input("file:///x/y.html").unwrap();
        assert_eq!(url.as_str(), "file:///x/y.html");
    }

    #[test]
    fn http_url_passes_through() {
        let url = base_url_from_input("http://example.com/").unwrap();
        assert_eq!(url.scheme(), "http");
        assert_eq!(url.as_str(), "http://example.com/");
    }

    #[test]
    fn join_relative_against_file_base() {
        let base = Url::parse("file:///a/b/page.html").unwrap();
        assert_eq!(base.join("theme.css").unwrap().as_str(), "file:///a/b/theme.css");
        assert_eq!(base.join("sub/i.png").unwrap().as_str(), "file:///a/b/sub/i.png");
        assert_eq!(base.join("/abs.css").unwrap().as_str(), "file:///abs.css");
        assert_eq!(
            base.join("http://h/x.css").unwrap().as_str(),
            "http://h/x.css"
        );
    }

    #[test]
    fn join_relative_against_dir_base() {
        // A trailing-slash directory base keeps the last segment when joining.
        let base = Url::parse("file:///a/b/").unwrap();
        assert_eq!(base.join("theme.css").unwrap().as_str(), "file:///a/b/theme.css");
        assert_eq!(base.join("img/p.png").unwrap().as_str(), "file:///a/b/img/p.png");
    }
}
