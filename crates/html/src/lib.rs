//! starfish-html — a hand-rolled HTML tokenizer + tree builder that turns an
//! HTML document string into a `starfish_dom::Document`. Pragmatic subset of
//! the WHATWG HTML spec; no JavaScript, no networking. See the M1 design note.

pub mod tokenizer;
mod tree_builder;

pub use starfish_dom::{Document, NodeId};
pub use tree_builder::parse;

#[cfg(test)]
mod tests {
    use super::parse;

    // Convenience: serialize the whole document for shape assertions.
    fn shape(html: &str) -> String {
        let doc = parse(html);
        doc.serialize(doc.root())
    }

    #[test]
    fn full_document() {
        let html = "<!DOCTYPE html><html><head><title>T</title></head><body><p>Hi</p></body></html>";
        assert_eq!(
            shape(html),
            "\
(document
  (doctype html)
  (element html
    (element head
      (element title
        \"T\"))
    (element body
      (element p
        \"Hi\"))))"
        );
    }

    #[test]
    fn implied_skeleton() {
        assert_eq!(
            shape("<p>hi"),
            "\
(document
  (element html
    (element head)
    (element body
      (element p
        \"hi\"))))"
        );
    }

    #[test]
    fn void_element() {
        // <br> is a leaf; "text" is a sibling of <br>, not its child.
        assert_eq!(
            shape("<div><br>text</div>"),
            "\
(document
  (element html
    (element head)
    (element body
      (element div
        (element br)
        \"text\"))))"
        );
    }

    #[test]
    fn auto_close_p() {
        assert_eq!(
            shape("<p>one<p>two"),
            "\
(document
  (element html
    (element head)
    (element body
      (element p
        \"one\")
      (element p
        \"two\"))))"
        );
    }

    #[test]
    fn auto_close_li() {
        assert_eq!(
            shape("<ul><li>a<li>b</ul>"),
            "\
(document
  (element html
    (element head)
    (element body
      (element ul
        (element li
          \"a\")
        (element li
          \"b\")))))"
        );
    }

    #[test]
    fn auto_close_li_across_block() {
        // The second <li> closes the first one even though a <div> is open
        // inside it; "b" lands in the second <li>, a sibling of the first.
        assert_eq!(
            shape("<ul><li><div>a<li>b</ul>"),
            "\
(document
  (element html
    (element head)
    (element body
      (element ul
        (element li
          (element div
            \"a\"))
        (element li
          \"b\")))))"
        );
    }

    #[test]
    fn nested_list_keeps_inner_items_inside_inner_list() {
        // A nested <ol> inside an <li>: the inner <li>x/<li>y belong to the
        // inner <ol> (not siblings of the outer items), and <li>b stays under
        // the outer <ol> after the inner list closes.
        assert_eq!(
            shape("<ol><li>a<ol><li>x<li>y</ol></li><li>b</ol>"),
            "\
(document
  (element html
    (element head)
    (element body
      (element ol
        (element li
          \"a\"
          (element ol
            (element li
              \"x\")
            (element li
              \"y\")))
        (element li
          \"b\")))))"
        );
    }

    #[test]
    fn auto_close_dt_dd_siblings() {
        assert_eq!(
            shape("<dl><dt>a<dd>b<dt>c</dl>"),
            "\
(document
  (element html
    (element head)
    (element body
      (element dl
        (element dt
          \"a\")
        (element dd
          \"b\")
        (element dt
          \"c\")))))"
        );
    }

    #[test]
    fn auto_close_option_across_inline() {
        // The second <option> closes the first even though a <b> is open
        // inside it; "y" lands in the second <option>.
        assert_eq!(
            shape("<select><option><b>x<option>y"),
            "\
(document
  (element html
    (element head)
    (element body
      (element select
        (element option
          (element b
            \"x\"))
        (element option
          \"y\")))))"
        );
    }

    #[test]
    fn text_coalescing_across_reference() {
        // a&amp;b -> single text node "a&b"
        let doc = parse("<p>a&amp;b");
        let root = doc.root();
        let html = doc.children(root)[0];
        let body = doc.children(html)[1];
        let p = doc.children(body)[0];
        let kids = doc.children(p);
        assert_eq!(kids.len(), 1);
        match doc.kind(kids[0]) {
            starfish_dom::NodeKind::Text(s) => assert_eq!(s, "a&b"),
            _ => panic!("expected single text node"),
        }
    }

    #[test]
    fn stray_end_tag() {
        // The unmatched </div> is ignored; <p> holds text "xy".
        assert_eq!(
            shape("<p>x</div>y</p>"),
            "\
(document
  (element html
    (element head)
    (element body
      (element p
        \"xy\"))))"
        );
    }

    #[test]
    fn attributes_reach_dom() {
        let doc = parse(r#"<a href="u">L</a>"#);
        let root = doc.root();
        let html = doc.children(root)[0];
        let body = doc.children(html)[1];
        let a = doc.children(body)[0];
        assert_eq!(doc.get_attribute(a, "href"), Some("u"));
    }

    #[test]
    fn comment_placement() {
        assert_eq!(
            shape("<body><!--c--></body>"),
            "\
(document
  (element html
    (element head)
    (element body
      (comment \"c\"))))"
        );
    }

    #[test]
    fn unclosed_at_eof() {
        assert_eq!(
            shape("<div><span>hi"),
            "\
(document
  (element html
    (element head)
    (element body
      (element div
        (element span
          \"hi\")))))"
        );
    }

    #[test]
    fn unterminated_comment_no_panic() {
        // should not panic; comment captured to EOF
        let doc = parse("<p><!-- oops");
        let _ = doc.serialize(doc.root());
    }

    #[test]
    fn typical_static_page_smoke() {
        let html = "<!DOCTYPE html><html><head><title>Page</title></head><body>\
<h1>Title</h1><p>A <a href=\"l\">link</a>.</p>\
<ul><li>one<li>two</ul><img src=\"i.png\"><div><div>nested</div></div></body></html>";
        let doc = parse(html);
        let out = doc.serialize(doc.root());
        // Spot-check structural anchors rather than the whole string.
        assert!(out.starts_with("(document\n  (doctype html)\n  (element html"));
        assert!(out.contains("(element h1\n"));
        assert!(out.contains("(element a href=\"l\"\n"));
        assert!(out.contains("(element li\n          \"one\")"));
        assert!(out.contains("(element img src=\"i.png\")"));
        assert!(out.contains("(element div\n        (element div\n          \"nested\")"));
    }
}
