//! Walk the DOM in document order → `Vec<ScriptSource>` (fully-resolved source
//! text ready to eval). Inline `<script>` text vs external `<script src>` fetched
//! through the existing `ResourceLoader`. `type` gating + non-fatal failures
//! (§3 of the design note).

use starfish_dom::{Document, NodeId, NodeKind};
use starfish_net::{ResourceLoader, Url};

/// One runnable classic script's source text.
pub(crate) struct ScriptSource {
    pub code: String,
}

/// Outcome of collecting a single external `<script src>`: either resolved
/// source bytes, or a recorded load-failure message (non-fatal).
enum External {
    Loaded(String),
    Failed(String),
}

/// Document-order DFS collecting every runnable `<script>`. External-load
/// failures are returned alongside as messages so `run_scripts` can record them
/// as `ScriptError`s; nothing here ever panics or aborts the walk.
pub(crate) fn collect_scripts(
    doc: &Document,
    base: &Url,
    loader: &dyn ResourceLoader,
) -> (Vec<ScriptSource>, Vec<String>) {
    let mut out = Vec::new();
    let mut load_errors = Vec::new();
    let mut stack = vec![doc.root()];
    // DFS; children pushed reversed so we pop them in document order.
    while let Some(id) = stack.pop() {
        if doc.tag_name(id) == Some("script") && is_runnable_type(doc.get_attribute(id, "type")) {
            match doc.get_attribute(id, "src") {
                // External: a <script src> ignores its inline text (HTML spec).
                Some(src) if !src.trim().is_empty() => {
                    match fetch_external(src.trim(), base, loader) {
                        External::Loaded(code) => out.push(ScriptSource { code }),
                        External::Failed(msg) => load_errors.push(msg),
                    }
                }
                // Inline: concatenate child Text nodes.
                _ => {
                    let code = inline_text(doc, id);
                    if !code.trim().is_empty() {
                        out.push(ScriptSource { code });
                    }
                }
            }
        }
        for c in doc.children(id).into_iter().rev() {
            stack.push(c);
        }
    }
    (out, load_errors)
}

/// Run a script iff its `type` is absent or a classic-script MIME type. A
/// `type="module"` or any other value (e.g. `application/json`) is skipped — not
/// an error, just not executed.
fn is_runnable_type(t: Option<&str>) -> bool {
    match t {
        None => true,
        Some(t) => {
            let t = t.trim().to_ascii_lowercase();
            matches!(
                t.as_str(),
                "" | "text/javascript"
                    | "application/javascript"
                    | "application/ecmascript"
                    | "text/ecmascript"
            )
        }
    }
}

/// Concatenate a `<script>`'s child `Text` payloads (its raw content).
fn inline_text(doc: &Document, id: NodeId) -> String {
    let mut s = String::new();
    for c in doc.children(id) {
        if let NodeKind::Text(t) = doc.kind(c) {
            s.push_str(t);
        }
    }
    s
}

/// Resolve `src` against `base`, fetch via `loader`, decode UTF-8 lossy. Any
/// resolution or fetch failure → `External::Failed("failed to load …")`.
fn fetch_external(src: &str, base: &Url, loader: &dyn ResourceLoader) -> External {
    let url = match base.join(src) {
        Ok(u) => u,
        Err(_) => return External::Failed(format!("failed to load script src {src:?}: bad URL")),
    };
    match loader.fetch(&url) {
        Ok(res) => External::Loaded(String::from_utf8_lossy(&res.bytes).into_owned()),
        Err(e) => External::Failed(format!("failed to load script {url}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runnable_type_gating() {
        assert!(is_runnable_type(None));
        assert!(is_runnable_type(Some("")));
        assert!(is_runnable_type(Some("text/javascript")));
        assert!(is_runnable_type(Some(" Application/JavaScript ")));
        assert!(is_runnable_type(Some("application/ecmascript")));
        assert!(!is_runnable_type(Some("module")));
        assert!(!is_runnable_type(Some("application/json")));
        assert!(!is_runnable_type(Some("text/coffeescript")));
    }
}
