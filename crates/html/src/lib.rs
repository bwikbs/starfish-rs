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
    use starfish_dom::{Document, NodeId};

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

    // --- E9-M1: inline SVG foreign content ---

    /// Walk to the <svg> element under body (skeleton: doc>html>[head,body]>svg).
    fn svg_of(doc: &Document) -> NodeId {
        let html = doc.children(doc.root())[0];
        let body = doc.children(html)[1];
        doc.children(body)
            .into_iter()
            .find(|&c| doc.tag_name(c) == Some("svg"))
            .expect("an <svg> element under body")
    }

    #[test]
    fn svg_self_closing_shapes_are_siblings() {
        let doc = parse(
            "<svg width=\"100\" height=\"100\">\
             <rect x=\"10\" y=\"10\" width=\"80\" height=\"80\" fill=\"red\"/>\
             <circle cx=\"50\" cy=\"50\" r=\"20\" fill=\"blue\"/></svg>",
        );
        let svg = svg_of(&doc);
        let kids: Vec<_> = doc
            .children(svg)
            .into_iter()
            .filter(|&c| doc.tag_name(c).is_some())
            .collect();
        // rect and circle are SIBLINGS (self-closed → not nested).
        assert_eq!(kids.len(), 2, "rect+circle siblings: {}", doc.serialize(svg));
        assert_eq!(doc.tag_name(kids[0]), Some("rect"));
        assert_eq!(doc.tag_name(kids[1]), Some("circle"));
        // attrs preserved on the rect.
        assert_eq!(doc.get_attribute(kids[0], "x"), Some("10"));
        assert_eq!(doc.get_attribute(kids[0], "width"), Some("80"));
        assert_eq!(doc.get_attribute(kids[0], "fill"), Some("red"));
        assert_eq!(doc.get_attribute(kids[1], "r"), Some("20"));
    }

    #[test]
    fn svg_viewbox_attribute_keeps_case() {
        let doc = parse("<svg viewBox=\"0 0 10 10\"><rect/></svg>");
        let svg = svg_of(&doc);
        // The case-preserved attribute name `viewBox` is readable; `viewbox` is not.
        assert_eq!(doc.get_attribute(svg, "viewBox"), Some("0 0 10 10"));
        assert_eq!(doc.get_attribute(svg, "viewbox"), None);
    }

    #[test]
    fn svg_exits_and_html_resumes() {
        // </svg> exits SVG mode; the following <p> is a normal HTML sibling.
        let doc = parse("<svg><circle/></svg><p>after");
        let html = doc.children(doc.root())[0];
        let body = doc.children(html)[1];
        let kids: Vec<_> = doc
            .children(body)
            .into_iter()
            .filter(|&c| doc.tag_name(c).is_some())
            .collect();
        assert_eq!(doc.tag_name(kids[0]), Some("svg"));
        assert_eq!(doc.tag_name(kids[1]), Some("p"));
        // the circle is the svg's child, not nesting the <p>.
        let svg = kids[0];
        let circle = doc.children(svg)[0];
        assert_eq!(doc.tag_name(circle), Some("circle"));
    }

    #[test]
    fn svg_non_self_closed_shape_closes_with_end_tag() {
        // A <rect>…</rect> (no slash) is fine: the matching </rect> pops it.
        let doc = parse("<svg><rect></rect><circle/></svg>");
        let svg = svg_of(&doc);
        let kids: Vec<_> = doc
            .children(svg)
            .into_iter()
            .filter(|&c| doc.tag_name(c).is_some())
            .collect();
        assert_eq!(kids.len(), 2);
        assert_eq!(doc.tag_name(kids[0]), Some("rect"));
        assert_eq!(doc.tag_name(kids[1]), Some("circle"));
    }

    #[test]
    fn svg_unclosed_then_div_close_clears_svg_mode() {
        // No explicit </svg>: the </div> truncates the stack past <svg>, so SVG
        // mode must clear. The following <img> is then lowercased HTML and void
        // (a sibling, not an uppercase nested svg child).
        let doc = parse("<div><svg><rect/></div><img src=x>tail");
        let html = doc.children(doc.root())[0];
        let body = doc.children(html)[1];
        let body_kids: Vec<_> = doc
            .children(body)
            .into_iter()
            .filter(|&c| doc.tag_name(c).is_some())
            .collect();
        // div then img are body-level siblings.
        assert_eq!(doc.tag_name(body_kids[0]), Some("div"));
        let img = body_kids[1];
        // lowercased (not "IMG") and void (no children).
        assert_eq!(doc.tag_name(img), Some("img"));
        assert_eq!(doc.get_attribute(img, "src"), Some("x"));
        assert!(
            doc.children(img).is_empty(),
            "img is void, not nesting tail: {}",
            doc.serialize(body)
        );
        // the svg (with its rect) is the div's child; nothing leaked out.
        let div = body_kids[0];
        let svg = doc.children(div)[0];
        assert_eq!(doc.tag_name(svg), Some("svg"));
    }

    #[test]
    fn svg_unclosed_then_body_end_clears_svg_mode() {
        // </body> while inside an unclosed <svg> pops past it, so the following
        // <DIV> is parsed as ordinary HTML (lowercased), not stuck in svg mode
        // (which would have stored it verbatim as "DIV").
        let doc = parse("<body><svg><rect/></body><DIV>x");
        let out = doc.serialize(doc.root());
        // The trailing tag is a lowercased HTML <div>, never a verbatim "DIV".
        assert!(out.contains("(element div"), "div lowercased: {out}");
        assert!(!out.contains("DIV"), "no verbatim SVG-mode leak: {out}");
        // The svg subtree (rect) stayed inside the svg, didn't swallow the div.
        fn find_svg(doc: &Document, n: NodeId) -> Option<NodeId> {
            if doc.tag_name(n) == Some("svg") {
                return Some(n);
            }
            doc.children(n).into_iter().find_map(|c| find_svg(doc, c))
        }
        let svg = find_svg(&doc, doc.root()).expect("svg present");
        let svg_kids: Vec<_> = doc
            .children(svg)
            .into_iter()
            .filter(|&c| doc.tag_name(c).is_some())
            .collect();
        assert_eq!(svg_kids.len(), 1);
        assert_eq!(doc.tag_name(svg_kids[0]), Some("rect"));
    }

    #[test]
    fn svg_wellformed_siblings_then_html_resumes() {
        // Regression: a well-formed <svg> still works — rect+circle siblings,
        // case preserved, and <p> is a sibling of <svg> back in HTML mode.
        let doc = parse("<svg><rect/><circle/></svg><p>after</p>");
        let html = doc.children(doc.root())[0];
        let body = doc.children(html)[1];
        let body_kids: Vec<_> = doc
            .children(body)
            .into_iter()
            .filter(|&c| doc.tag_name(c).is_some())
            .collect();
        assert_eq!(doc.tag_name(body_kids[0]), Some("svg"));
        assert_eq!(doc.tag_name(body_kids[1]), Some("p"));
        let svg = body_kids[0];
        let shape_kids: Vec<_> = doc
            .children(svg)
            .into_iter()
            .filter(|&c| doc.tag_name(c).is_some())
            .collect();
        assert_eq!(shape_kids.len(), 2);
        assert_eq!(doc.tag_name(shape_kids[0]), Some("rect"));
        assert_eq!(doc.tag_name(shape_kids[1]), Some("circle"));
    }

    #[test]
    fn svg_nested_svg_stays_in_svg_until_both_pop() {
        // A nested <svg> inside an <svg>: in_svg() stays true until both pop.
        // The inner <rect> (after the inner </svg>) is still SVG content; the
        // trailing <p> after the outer </svg> is HTML again.
        let doc = parse("<svg><svg><circle/></svg><rect/></svg><p>x</p>");
        let html = doc.children(doc.root())[0];
        let body = doc.children(html)[1];
        let body_kids: Vec<_> = doc
            .children(body)
            .into_iter()
            .filter(|&c| doc.tag_name(c).is_some())
            .collect();
        // outer svg then p (HTML resumed after outer </svg>).
        assert_eq!(doc.tag_name(body_kids[0]), Some("svg"));
        assert_eq!(doc.tag_name(body_kids[1]), Some("p"));
        let outer = body_kids[0];
        let outer_kids: Vec<_> = doc
            .children(outer)
            .into_iter()
            .filter(|&c| doc.tag_name(c).is_some())
            .collect();
        // inner svg, then rect (still SVG content after inner </svg>).
        assert_eq!(doc.tag_name(outer_kids[0]), Some("svg"));
        assert_eq!(doc.tag_name(outer_kids[1]), Some("rect"));
    }

    #[test]
    fn non_svg_page_parses_identically() {
        // A page with no <svg> is byte-identical to before (no-regression).
        assert_eq!(
            shape("<div><p>hi</p><br>x</div>"),
            "\
(document
  (element html
    (element head)
    (element body
      (element div
        (element p
          \"hi\")
        (element br)
        \"x\"))))"
        );
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
