//! starfish-js (E4-M1) — integrate the Boa pure-Rust JS engine so a page's
//! `<script>`s (inline + external via the existing `ResourceLoader`) execute in
//! document order against one shared `Context`; `console.{log,info,warn,error,
//! debug}` are captured; minimal `window`/`navigator`/`location`/`document`
//! globals exist so common scripts don't `ReferenceError`. Script errors are
//! NON-FATAL — captured as values, never propagated. See `docs/design/E4-M1.md`.
//!
//! NO DOM bindings in M1 beyond a read-only `document.{title,URL}` probe; events
//! and timers are deferred (M2/M3). The `&mut Document` signature + the internal
//! `Rc<RefCell<Document>>` seam are wired now so M2 slots in without re-plumbing
//! style/layout/paint.

use std::cell::RefCell;
use std::rc::Rc;

use boa_engine::{Context, Source};
use starfish_dom::Document;
use starfish_net::{ResourceLoader, Url};

mod collect;
mod console;
mod globals;

/// One captured `console` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleMessage {
    pub level: ConsoleLevel,
    /// Already-stringified, space-joined arguments.
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleLevel {
    Log,
    Info,
    Warn,
    Error,
    Debug,
}

/// One uncaught script error (a thrown value or a load failure), captured —
/// never propagated. `message` is the engine's display of the thrown value or a
/// "failed to load …" message; `src` carries the offending source URL when
/// known (a load failure). Execution continues with the next script.
#[derive(Debug, Clone)]
pub struct ScriptError {
    pub message: String,
    pub src: Option<String>,
}

/// What [`run_scripts`] produces. Purely observational in M1.
#[derive(Debug, Default)]
pub struct ScriptOutcome {
    /// Captured console calls in global call order.
    pub console: Vec<ConsoleMessage>,
    /// Captured errors in script-encounter order.
    pub errors: Vec<ScriptError>,
    /// Number of `<script>` elements actually executed (skipped modules / failed
    /// loads not counted).
    pub executed: usize,
}

/// Execute every `<script>` in `doc` in document order against a single shared
/// Boa context, fetching external scripts (`<script src>`) against `base`
/// through `loader`.
///
/// NEVER panics, NEVER returns `Err`: a script that throws or a `src` that fails
/// to load is captured into the returned [`ScriptOutcome`] and execution
/// proceeds with the next script.
///
/// Ownership seam (M2): the `Document` is moved into an internal
/// `Rc<RefCell<Document>>` for the duration of scripting and reclaimed
/// afterwards. M1 does not mutate the DOM (no host object holds a clone), but the
/// seam is built + torn down here so M2 can hand Boa host objects a
/// `Rc::clone(&shared)` without re-plumbing this function or `render_document`.
pub fn run_scripts(doc: &mut Document, base: &Url, loader: &dyn ResourceLoader) -> ScriptOutcome {
    // 1. Collect script sources up front (immutable walk), so document order is
    //    fixed before any script could mutate the tree (M2).
    let (scripts, load_errors) = collect::collect_scripts(doc, base, loader);
    if scripts.is_empty() && load_errors.is_empty() {
        // Zero overhead for the common script-free page: no Context built.
        return ScriptOutcome::default();
    }

    // 2. Move the Document into a shared cell for the duration of scripting.
    //    `mem::take` leaves `*doc` a valid empty Document while JS runs.
    let shared: Rc<RefCell<Document>> = Rc::new(RefCell::new(std::mem::take(doc)));

    // 3. Build the Boa context, register the capturing console + globals
    //    (globals reads `shared` for the document.title probe; M2 stores a clone).
    let mut ctx = Context::default();
    let console_sink = console::install(&mut ctx);
    globals::install(&mut ctx, &shared, base);

    // 4. Execute each script in order in the SHARED context. A throw is captured,
    //    not propagated; the next script still runs.
    let mut errors: Vec<ScriptError> = load_errors
        .into_iter()
        .map(|m| ScriptError {
            message: m,
            src: None,
        })
        .collect();
    let mut executed = 0;
    for src in scripts {
        match ctx.eval(Source::from_bytes(src.code.as_bytes())) {
            Ok(_) => executed += 1,
            Err(e) => errors.push(ScriptError {
                message: format!("{e}"),
                src: None,
            }),
        }
    }

    // 5. Reclaim the Document. Drop the context first so no GC object still holds
    //    a clone of `shared` (M2: the document host object), making try_unwrap
    //    succeed. M1 has no live handle, so `expect` is sound (the deep-copy
    //    fallback is an M2 addition, documented, not built now).
    drop(ctx);
    debug_assert_eq!(
        Rc::strong_count(&shared),
        1,
        "a JS handle still holds the Document after the context dropped (M2 needs the deep-copy fallback)"
    );
    let recovered = Rc::try_unwrap(shared)
        .map(RefCell::into_inner)
        .unwrap_or_else(|_| panic!("no live JS handle to Document expected in M1"));
    *doc = recovered;

    ScriptOutcome {
        console: console_sink.take(),
        errors,
        executed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starfish_net::{file_url_from_path, LocalLoader};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn run(html: &str) -> (Document, ScriptOutcome) {
        let mut doc = starfish_html::parse(html);
        let base = Url::parse("file:///x/index.html").unwrap();
        let outcome = run_scripts(&mut doc, &base, &LocalLoader);
        (doc, outcome)
    }

    fn temp_dir() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("starfish-e4m1-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn base_in(dir: &Path) -> Url {
        file_url_from_path(&dir.join("index.html")).unwrap()
    }

    #[test]
    fn inline_console_log_captured() {
        let (_doc, out) = run("<script>console.log('hi')</script>");
        assert_eq!(
            out.console,
            vec![ConsoleMessage {
                level: ConsoleLevel::Log,
                text: "hi".into()
            }]
        );
        assert_eq!(out.executed, 1);
        assert!(out.errors.is_empty());
    }

    #[test]
    fn console_levels_mapped() {
        let (_doc, out) = run("<script>console.warn('w'); console.error('e'); console.info('i')</script>");
        let levels: Vec<_> = out.console.iter().map(|m| m.level).collect();
        assert_eq!(
            levels,
            vec![ConsoleLevel::Warn, ConsoleLevel::Error, ConsoleLevel::Info]
        );
        let texts: Vec<_> = out.console.iter().map(|m| m.text.as_str()).collect();
        assert_eq!(texts, vec!["w", "e", "i"]);
    }

    #[test]
    fn multi_arg_stringify_space_joined() {
        let (_doc, out) = run("<script>console.log('x', 1, true)</script>");
        assert_eq!(out.console[0].text, "x 1 true");
    }

    #[test]
    fn two_scripts_share_global_state() {
        // First declares `var a` + a window global; second reads both.
        let (_doc, out) = run(
            "<script>var a = 41; window.b = 1;</script><script>console.log(a + b)</script>",
        );
        assert_eq!(out.console.len(), 1);
        assert_eq!(out.console[0].text, "42");
        assert_eq!(out.executed, 2);
    }

    #[test]
    fn external_script_runs() {
        let dir = temp_dir();
        std::fs::write(dir.join("app.js"), "console.log('ext')").unwrap();
        let mut doc = starfish_html::parse("<script src='app.js'></script>");
        let out = run_scripts(&mut doc, &base_in(&dir), &LocalLoader);
        assert_eq!(out.console[0].text, "ext");
        assert_eq!(out.executed, 1);
        assert!(out.errors.is_empty());
    }

    #[test]
    fn failed_external_load_is_non_fatal() {
        let dir = temp_dir();
        let mut doc = starfish_html::parse(
            "<script src='missing.js'></script><script>console.log('after')</script>",
        );
        let out = run_scripts(&mut doc, &base_in(&dir), &LocalLoader);
        // Missing one skipped (recorded as an error), `after` still runs.
        assert_eq!(out.console, vec![ConsoleMessage {
            level: ConsoleLevel::Log,
            text: "after".into()
        }]);
        assert_eq!(out.executed, 1);
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].message.contains("missing.js"));
    }

    #[test]
    fn throwing_script_is_non_fatal_later_runs() {
        let (_doc, out) = run(
            "<script>throw new Error('boom')</script><script>console.log('later')</script>",
        );
        assert_eq!(out.errors.len(), 1);
        assert!(out.errors[0].message.contains("boom"));
        assert_eq!(out.console, vec![ConsoleMessage {
            level: ConsoleLevel::Log,
            text: "later".into()
        }]);
        assert_eq!(out.executed, 1);
    }

    #[test]
    fn globals_no_reference_error() {
        let (_doc, out) = run(
            "<script>console.log(typeof window, typeof globalThis, navigator.userAgent, location.href, document.URL)</script>",
        );
        assert!(out.errors.is_empty(), "errors: {:?}", out.errors);
        let line = &out.console[0].text;
        assert!(line.contains("object object"), "typeofs: {line}");
        assert!(line.contains("starfish-rs"), "UA present: {line}");
        assert!(line.contains("file:///x/index.html"), "href present: {line}");
    }

    #[test]
    fn window_is_global_this() {
        let (_doc, out) = run("<script>console.log(window === globalThis)</script>");
        assert!(out.errors.is_empty());
        assert_eq!(out.console[0].text, "true");
    }

    #[test]
    fn document_title_probe() {
        let (_doc, out) =
            run("<title>Hello</title><script>console.log(document.title)</script>");
        assert_eq!(out.console[0].text, "Hello");
    }

    #[test]
    fn module_and_data_types_skipped() {
        let (_doc, out) = run(
            "<script type='module'>console.log('m')</script>\
             <script type='application/json'>{}</script>\
             <script type='text/coffeescript'>console.log('c')</script>\
             <script type='text/javascript'>console.log('classic')</script>",
        );
        assert_eq!(out.executed, 1, "only the classic script runs");
        assert_eq!(out.console.len(), 1);
        assert_eq!(out.console[0].text, "classic");
    }

    #[test]
    fn no_scripts_empty_outcome() {
        let (_doc, out) = run("<p>hi</p>");
        assert!(out.console.is_empty());
        assert!(out.errors.is_empty());
        assert_eq!(out.executed, 0);
    }

    #[test]
    fn document_survives_round_trip() {
        let html = "<html><head><title>t</title></head><body><p>hi</p>\
                    <script>var x = 1;</script></body></html>";
        let mut doc = starfish_html::parse(html);
        let before = doc.serialize(doc.root());
        let base = Url::parse("file:///x/index.html").unwrap();
        let _ = run_scripts(&mut doc, &base, &LocalLoader);
        let after = doc.serialize(doc.root());
        // M1 does not mutate the DOM: the arena is reclaimed losslessly.
        assert_eq!(before, after);
    }
}
