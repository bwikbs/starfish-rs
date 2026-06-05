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

use boa_engine::{Context, JsValue, Source};
use starfish_dom::{Document, NodeId};
use starfish_net::{ResourceLoader, Url};

mod collect;
mod console;
mod dom;
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
    if scripts.is_empty() && load_errors.is_empty() && !has_inline_handler(doc) {
        // Zero overhead for the common script-free, handler-free page: no Context
        // built. A page with only an inline `on*` handler (e.g. `<body onload>`)
        // still needs the load sequence, so it falls through.
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

    // 4b. Run to quiescence (E4-M3): drain microtasks, fire DOMContentLoaded +
    //     load, then drain the bounded timer queue (microtasks after each step).
    //     Every callback mutates the same `shared` arena; all throws are caught.
    run_to_quiescence(&mut ctx, &shared, &mut errors);

    // 5. Reclaim the Document. Drop the context first so most/all GC objects
    //    holding a clone of `shared` (the DOM host objects + wrapper cache) are
    //    released. The fast path (sole owner) moves the arena out; if Boa's GC
    //    left a live clone (deferred sweep / cycle), the robust path deep-copies
    //    the post-script arena out. Either way `*doc` ends with every mutation,
    //    and we NEVER panic.
    drop(ctx);
    let recovered: Document = match Rc::try_unwrap(shared) {
        Ok(cell) => cell.into_inner(),
        Err(rc) => rc.borrow().clone(),
    };
    *doc = recovered;

    ScriptOutcome {
        console: console_sink.take(),
        errors,
        executed,
    }
}

/// Does the doc have any element bearing an `on*` attribute the load sequence
/// fires (currently only `onload`)? Cheap DFS — keeps the script-free,
/// handler-free page at zero overhead while honoring `<body onload>`.
fn has_inline_handler(doc: &Document) -> bool {
    use starfish_dom::{Element, NodeKind};
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        if let NodeKind::Element(Element { attrs, .. }) = doc.kind(id) {
            if attrs.iter().any(|a| a.name == "onload") {
                return true;
            }
        }
        for c in doc.children(id) {
            stack.push(c);
        }
    }
    false
}

/// Drain Boa's promise-reaction microtask queue; a throwing job is captured.
fn run_microtasks(ctx: &mut Context, errors: &mut Vec<ScriptError>) {
    if let Err(e) = ctx.run_jobs() {
        errors.push(ScriptError {
            message: format!("{e}"),
            src: None,
        });
    }
}

/// First `<body>` element id (for the `<body onload>` → window `load` mapping).
fn find_body(doc: &Document) -> Option<NodeId> {
    let mut stack = vec![doc.root()];
    while let Some(id) = stack.pop() {
        if doc.tag_name(id) == Some("body") {
            return Some(id);
        }
        for c in doc.children(id) {
            stack.push(c);
        }
    }
    None
}

/// The run-to-quiescence sequence (design §4.4): microtasks → DOMContentLoaded
/// (document) → microtasks → load (window) → microtasks → bounded timer drain
/// (microtasks after each callback). Never panics; never hangs.
fn run_to_quiescence(
    ctx: &mut Context,
    shared: &Rc<RefCell<Document>>,
    errors: &mut Vec<ScriptError>,
) {
    use dom::{event, WINDOW_KEY};

    // (a) microtasks from the initial scripts.
    run_microtasks(ctx, errors);

    // (b) DOMContentLoaded on document (the arena root, index 0).
    let root = shared.borrow().root();
    if let Ok(doc_val) = dom::wrap_node(root, ctx).map(JsValue::from) {
        let ev = event::build_event("DOMContentLoaded", false, ctx);
        let _ = ev.set(boa_engine::js_string!("target"), doc_val.clone(), false, ctx);
        // document has no `onDOMContentLoaded` content attribute → listeners only.
        event::fire_for_index(root.index(), "DOMContentLoaded", &ev, &doc_val, ctx);
    }
    run_microtasks(ctx, errors);

    // (c) load on window. `<body onload>` maps here (HTML spec).
    {
        let win = ctx.global_object();
        let win_val = JsValue::from(win);
        let ev = event::build_event("load", false, ctx);
        let _ = ev.set(boa_engine::js_string!("target"), win_val.clone(), false, ctx);
        event::fire_for_index(WINDOW_KEY, "load", &ev, &win_val, ctx);
        let body = find_body(&shared.borrow());
        if let Some(body) = body {
            let bh = dom::NodeHandle {
                shared: shared.clone(),
                id: body,
            };
            event::run_inline_handler(&bh, "load", &ev, &win_val, ctx);
        }
    }
    run_microtasks(ctx, errors);

    // (d) bounded timer drain in virtual-time order; microtasks after each.
    while let Some(step) = dom::timer::next_due_timer(ctx) {
        let _ = step.callback.call(&JsValue::undefined(), &[], ctx);
        run_microtasks(ctx, errors);
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

    // --- E4-M2 DOM bindings ---

    /// Run a page whose single inline script logs a value; return that log line.
    fn log_of(html: &str) -> String {
        let (_doc, out) = run(html);
        assert!(out.errors.is_empty(), "script errors: {:?}", out.errors);
        assert!(!out.console.is_empty(), "no console output");
        out.console[0].text.clone()
    }

    #[test]
    fn dom_get_element_by_id_and_tag_name() {
        let line = log_of(
            "<div id='x'>hi</div>\
             <script>var e=document.getElementById('x');\
             console.log(e.tagName, e.nodeName, e.textContent)</script>",
        );
        assert_eq!(line, "DIV DIV hi");
    }

    #[test]
    fn dom_get_element_by_id_miss_null() {
        assert_eq!(
            log_of("<script>console.log(document.getElementById('nope')===null)</script>"),
            "true"
        );
    }

    #[test]
    fn dom_text_content_nested() {
        assert_eq!(
            log_of("<p id='p'>a<span>b</span></p><script>console.log(document.getElementById('p').textContent)</script>"),
            "ab"
        );
    }

    #[test]
    fn dom_identity_cache() {
        // el === el.parentNode.firstChild (same wrapper from the cache).
        let line = log_of(
            "<div id='d'><span id='s'>x</span></div>\
             <script>var s=document.getElementById('s');\
             console.log(s===s.parentNode.firstChild)</script>",
        );
        assert_eq!(line, "true");
    }

    #[test]
    fn dom_set_attribute_reflects() {
        let (doc, out) = run(
            "<div id='x'>hi</div>\
             <script>document.getElementById('x').setAttribute('data-y','7')</script>",
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let x = find_id(&doc, "x");
        assert_eq!(doc.get_attribute(x, "data-y"), Some("7"));
    }

    #[test]
    fn dom_create_and_append() {
        let (doc, out) = run(
            "<div id='x'></div>\
             <script>var c=document.createElement('b');\
             c.textContent='hey';\
             document.getElementById('x').appendChild(c)</script>",
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let x = find_id(&doc, "x");
        let kids = doc.children(x);
        assert_eq!(kids.len(), 1);
        assert_eq!(doc.tag_name(kids[0]), Some("b"));
        assert_eq!(doc.serialize(kids[0]).trim(), "(element b\n  \"hey\")");
    }

    #[test]
    fn dom_remove_child() {
        let (doc, out) = run(
            "<div id='x'><i id='i'>z</i></div>\
             <script>var x=document.getElementById('x');\
             x.removeChild(document.getElementById('i'))</script>",
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(doc.children(find_id(&doc, "x")).len(), 0);
    }

    #[test]
    fn dom_text_content_set_replaces() {
        let (doc, out) = run(
            "<p id='p'>old<span>x</span></p>\
             <script>document.getElementById('p').textContent='new'</script>",
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let p = find_id(&doc, "p");
        let kids = doc.children(p);
        assert_eq!(kids.len(), 1);
        match doc.kind(kids[0]) {
            starfish_dom::NodeKind::Text(t) => assert_eq!(t, "new"),
            _ => panic!("expected single text child"),
        }
    }

    #[test]
    fn dom_query_selector_variants() {
        let html = "<div id='wrap'><p class='cls'>x</p><span><p>deep</p></span></div>\
            <script>\
            console.log(\
              document.querySelector('#wrap').tagName,\
              document.querySelector('.cls').textContent,\
              document.querySelector('div p').textContent,\
              document.querySelector('div>p').textContent,\
              document.querySelectorAll('p').length,\
              document.querySelector(':hover')===null\
            )</script>";
        assert_eq!(log_of(html), "DIV x x x 2 true");
    }

    #[test]
    fn dom_class_list_ops() {
        let line = log_of(
            "<div id='x' class='a'>hi</div>\
             <script>var c=document.getElementById('x').classList;\
             c.add('b'); c.add('a');\
             var r1=c.contains('b');\
             c.remove('a');\
             var r2=c.contains('a');\
             var r3=c.toggle('z');\
             console.log(r1,r2,r3,document.getElementById('x').className)</script>",
        );
        assert_eq!(line, "true false true b z");
    }

    #[test]
    fn dom_style_writes_attribute() {
        let (doc, out) = run(
            "<div id='x'>hi</div>\
             <script>document.getElementById('x').style.background='#00ff00'</script>",
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let x = find_id(&doc, "x");
        let style = doc.get_attribute(x, "style").unwrap_or("");
        assert!(style.contains("background"), "style was {style:?}");
        assert!(style.contains("#00ff00"), "style was {style:?}");
    }

    #[test]
    fn dom_remove_child_nonchild_throws_nonfatal() {
        let (doc, out) = run(
            "<div id='a'>x</div><div id='b'>y</div>\
             <script>document.getElementById('a').removeChild(document.getElementById('b'))</script>\
             <script>console.log('after')</script>",
        );
        assert_eq!(out.errors.len(), 1, "expected one thrown error");
        // the page still renders / second script runs.
        assert_eq!(out.console.last().unwrap().text, "after");
        // arena well-formed.
        assert!(doc.serialize(doc.root()).contains("(document"));
    }

    #[test]
    fn dom_cycle_append_throws() {
        let (_doc, out) = run(
            "<div id='a'><div id='b'>x</div></div>\
             <script>document.getElementById('b').appendChild(document.getElementById('a'))</script>",
        );
        assert_eq!(out.errors.len(), 1, "cycle must throw");
    }

    #[test]
    fn dom_clone_out_with_leaked_handle() {
        // A script stashes a global reference to a node so a wrapper survives;
        // the clone-out fallback must still recover the (mutated) arena.
        let (doc, out) = run(
            "<body id='b'><p>hi</p></body>\
             <script>window.keep=document.getElementById('b');\
             window.keep.setAttribute('data-z','héllo🌟')</script>",
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let b = find_id(&doc, "b");
        assert_eq!(doc.get_attribute(b, "data-z"), Some("héllo🌟"));
        assert!(doc.serialize(doc.root()).contains("(document"));
    }

    fn find_id(doc: &Document, id: &str) -> starfish_dom::NodeId {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.get_attribute(n, "id") == Some(id) {
                return n;
            }
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        panic!("no element with id={id}");
    }

    // --- E4-M3 events + timers + load sequence ---

    /// All console lines (in order) of a page that must run without errors.
    fn lines_of(html: &str) -> Vec<String> {
        let (_doc, out) = run(html);
        assert!(out.errors.is_empty(), "script errors: {:?}", out.errors);
        out.console.iter().map(|m| m.text.clone()).collect()
    }

    #[test]
    fn add_and_dispatch_fires_listener() {
        let lines = lines_of(
            "<div id='x'></div>\
             <script>var e=document.getElementById('x');\
             e.addEventListener('go', function(){ console.log('fired'); });\
             e.dispatchEvent(new Event('go'));</script>",
        );
        assert_eq!(lines, vec!["fired"]);
    }

    #[test]
    fn dispatch_sets_target_and_type() {
        let lines = lines_of(
            "<div id='x'></div>\
             <script>var e=document.getElementById('x');\
             e.addEventListener('go', function(ev){ console.log(ev.type, ev.target===e, this===e); });\
             e.dispatchEvent(new Event('go'));</script>",
        );
        assert_eq!(lines, vec!["go true true"]);
    }

    #[test]
    fn dedupe_same_listener() {
        let lines = lines_of(
            "<div id='x'></div>\
             <script>var e=document.getElementById('x');\
             var f=function(){ console.log('once'); };\
             e.addEventListener('go', f); e.addEventListener('go', f);\
             e.dispatchEvent(new Event('go'));</script>",
        );
        assert_eq!(lines, vec!["once"]);
    }

    #[test]
    fn remove_event_listener_stops_it() {
        let lines = lines_of(
            "<div id='x'></div>\
             <script>var e=document.getElementById('x');\
             var f=function(){ console.log('no'); };\
             e.addEventListener('go', f); e.removeEventListener('go', f);\
             e.dispatchEvent(new Event('go'));\
             console.log('done');</script>",
        );
        assert_eq!(lines, vec!["done"]);
    }

    #[test]
    fn remove_during_dispatch_is_safe() {
        // Removing a not-yet-fired listener during dispatch must not panic; the
        // snapshot semantics mean it still runs this dispatch (documented).
        let lines = lines_of(
            "<div id='x'></div>\
             <script>var e=document.getElementById('x');\
             var b=function(){ console.log('b'); };\
             var a=function(){ console.log('a'); e.removeEventListener('go', b); };\
             e.addEventListener('go', a); e.addEventListener('go', b);\
             e.dispatchEvent(new Event('go'));\
             e.dispatchEvent(new Event('go'));</script>",
        );
        // first dispatch: a,b (b snapshotted); second dispatch: a only.
        assert_eq!(lines, vec!["a", "b", "a"]);
    }

    #[test]
    fn bubble_and_stop_propagation() {
        let lines = lines_of(
            "<div id='p'><span id='c'></span></div>\
             <script>var p=document.getElementById('p'), c=document.getElementById('c');\
             p.addEventListener('go', function(ev){ console.log('p', ev.currentTarget===p, ev.target===c); });\
             c.dispatchEvent(new Event('go'));</script>",
        );
        assert_eq!(lines, vec!["p true true"]);

        let stopped = lines_of(
            "<div id='p'><span id='c'></span></div>\
             <script>var p=document.getElementById('p'), c=document.getElementById('c');\
             p.addEventListener('go', function(){ console.log('p'); });\
             c.addEventListener('go', function(ev){ ev.stopPropagation(); });\
             c.dispatchEvent(new Event('go'));\
             console.log('end');</script>",
        );
        assert_eq!(stopped, vec!["end"]);
    }

    #[test]
    fn prevent_default_sets_flag() {
        let lines = lines_of(
            "<div id='x'></div>\
             <script>var e=document.getElementById('x'), ev=new Event('go');\
             e.addEventListener('go', function(ev){ ev.preventDefault(); });\
             var r=e.dispatchEvent(ev);\
             console.log(ev.defaultPrevented, r);</script>",
        );
        assert_eq!(lines, vec!["true false"]);
    }

    #[test]
    fn throwing_listener_is_non_fatal() {
        let (_doc, out) = run(
            "<div id='x'></div>\
             <script>var e=document.getElementById('x');\
             e.addEventListener('go', function(){ throw new Error('boom'); });\
             e.addEventListener('go', function(){ console.log('after'); });\
             e.dispatchEvent(new Event('go'));</script>",
        );
        assert_eq!(out.console.last().unwrap().text, "after");
    }

    #[test]
    fn dom_content_loaded_fires_after_scripts() {
        let (doc, out) = run(
            "<body><p id='p'>hi</p></body>\
             <script>document.addEventListener('DOMContentLoaded', function(){\
               document.getElementById('p').style.background='#00ff00'; });</script>",
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let p = find_id(&doc, "p");
        let style = doc.get_attribute(p, "style").unwrap_or("");
        assert!(style.contains("#00ff00"), "style was {style:?}");
    }

    #[test]
    fn load_fires_on_window() {
        let lines = lines_of(
            "<script>window.addEventListener('load', function(){ console.log('loaded'); });</script>",
        );
        assert_eq!(lines, vec!["loaded"]);
    }

    #[test]
    fn inline_body_onload_runs_without_script() {
        // No <script> at all — only an inline handler. The early-return guard
        // must still build the context and fire load.
        let (doc, out) = run("<body onload=\"document.body.setAttribute('data-x','1')\"><p>hi</p></body>");
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let body = find_id_by_tag(&doc, "body");
        assert_eq!(doc.get_attribute(body, "data-x"), Some("1"));
    }

    #[test]
    fn set_timeout_zero_mutates_dom() {
        let (doc, out) = run(
            "<div id='b'>hi</div>\
             <script>setTimeout(function(){ document.getElementById('b').setAttribute('data-t','1'); }, 0);</script>",
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        let b = find_id(&doc, "b");
        assert_eq!(doc.get_attribute(b, "data-t"), Some("1"));
    }

    #[test]
    fn set_interval_bounded_no_hang() {
        let lines = lines_of(
            "<script>var n=0; var id=setInterval(function(){ n++; if(n>=1000000) clearInterval(id); }, 0);\
             window.addEventListener('load', function(){ /* runs before timers */ });</script>\
             <script>window.addEventListener('load', function(){});</script>",
        );
        let _ = lines;
        // If we got here the drain terminated (bounded). Assert n <= cap via a probe.
        let probe = lines_of(
            "<script>var n=0; var id=setInterval(function(){ n++; if(n>=1000000) clearInterval(id); }, 0);\
             setTimeout(function(){ console.log(n<=10000); }, 50);</script>",
        );
        assert_eq!(probe.last().unwrap(), "true");
    }

    #[test]
    fn clear_timeout_cancels() {
        let lines = lines_of(
            "<script>var id=setTimeout(function(){ console.log('no'); }, 5); clearTimeout(id);\
             clearTimeout(99999);\
             setTimeout(function(){ console.log('yes'); }, 0);</script>",
        );
        assert_eq!(lines, vec!["yes"]);
    }

    #[test]
    fn nested_set_timeout_drains() {
        let lines = lines_of(
            "<script>setTimeout(function(){ setTimeout(function(){ console.log('inner'); }, 0); }, 0);</script>",
        );
        assert_eq!(lines, vec!["inner"]);
    }

    #[test]
    fn virtual_time_ordering() {
        let lines = lines_of(
            "<script>setTimeout(function(){ console.log('a'); }, 50);\
             setTimeout(function(){ console.log('b'); }, 10);</script>",
        );
        assert_eq!(lines, vec!["b", "a"]);
    }

    #[test]
    fn throwing_timer_is_non_fatal() {
        let (_doc, out) = run(
            "<script>setTimeout(function(){ throw new Error('x'); }, 0);\
             setTimeout(function(){ console.log('ok'); }, 0);</script>",
        );
        assert_eq!(out.console.last().unwrap().text, "ok");
    }

    #[test]
    fn promise_microtask_runs() {
        let lines = lines_of(
            "<script>Promise.resolve().then(function(){ console.log('micro'); });</script>",
        );
        assert_eq!(lines, vec!["micro"]);
    }

    #[test]
    fn microtask_before_next_timer() {
        // A setTimeout whose callback resolves a promise whose .then logs → the
        // .then runs before the next timer.
        let lines = lines_of(
            "<script>\
             setTimeout(function(){ console.log('t1'); Promise.resolve().then(function(){ console.log('p'); }); }, 0);\
             setTimeout(function(){ console.log('t2'); }, 1);</script>",
        );
        assert_eq!(lines, vec!["t1", "p", "t2"]);
    }

    fn find_id_by_tag(doc: &Document, tag: &str) -> NodeId {
        let mut stack = vec![doc.root()];
        while let Some(n) = stack.pop() {
            if doc.tag_name(n) == Some(tag) {
                return n;
            }
            for c in doc.children(n) {
                stack.push(c);
            }
        }
        panic!("no <{tag}>");
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
