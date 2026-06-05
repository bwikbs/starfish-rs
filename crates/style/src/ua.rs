//! User-Agent default stylesheet (§6). Parsed via starfish-css at startup.

use starfish_css::{parse_stylesheet, Stylesheet};

const UA_CSS: &str = r#"
html, body, div, p, section, article, header, footer, nav, main, aside,
h1, h2, h3, h4, h5, h6, ul, ol, li, dl, dd, blockquote, pre, table,
figure, figcaption, address, hr, form { display: block }

span, a, b, i, em, strong, small, code, label, abbr, cite, q, sub, sup,
u, s, mark, br { display: inline }

img, button, input, select, textarea { display: inline-block }

head, title, meta, link, style, script, base { display: none }

body   { margin: 8px }
p      { margin: 16px 0 }
h1     { margin: 21px 0; font-size: 32px; font-weight: bold }
h2     { margin: 19px 0; font-size: 24px; font-weight: bold }
h3     { margin: 18px 0; font-size: 18px; font-weight: bold }
h4     { margin: 21px 0; font-weight: bold }
h5     { margin: 22px 0; font-size: 13px; font-weight: bold }
h6     { margin: 24px 0; font-size: 11px; font-weight: bold }
ul, ol { margin: 16px 0; padding-left: 40px }
b, strong { font-weight: bold }
"#;

/// Parse the UA default stylesheet.
pub(crate) fn ua_stylesheet() -> Stylesheet {
    parse_stylesheet(UA_CSS)
}
