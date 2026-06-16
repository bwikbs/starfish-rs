//! User-Agent default stylesheet (§6). Parsed via starfish-css at startup.

use starfish_css::{parse_stylesheet, Stylesheet};

const UA_CSS: &str = r#"
html, body, div, p, section, article, header, footer, nav, main, aside,
h1, h2, h3, h4, h5, h6, ul, ol, li, dl, dd, blockquote, pre,
figure, figcaption, address, hr, form { display: block }

table   { display: table; border-collapse: separate; border-spacing: 2px }
tr      { display: table-row }
td, th  { display: table-cell }
thead, tbody, tfoot { display: table-row-group }
th      { font-weight: bold; text-align: center }
caption { display: block }

span, a, b, i, em, strong, small, code, label, abbr, cite, q, sub, sup,
u, s, mark, br { display: inline }

img, button, input, select, textarea { display: inline-block }

slot { display: contents } /* E34-M1: slotted content lays out in the parent flow */

details, summary { display: block } /* E36-M1 */

/* E36-M2: a <dialog> is block when open, hidden otherwise; the global `hidden`
   attribute hides any element. An open dialog gets a bordered, padded box. */
dialog { display: block; border: 2px solid #767676; padding: 16px; background: #fff }
dialog:not([open]) { display: none }
[hidden] { display: none }

/* E36-M3: a [popover] element is hidden until its internal open flag is set
   (via showPopover/togglePopover, reflected by :popover-open). The open one
   gets a bordered, padded box; :popover-open outspecs [popover] so it wins. */
[popover] { display: none }
[popover]:popover-open { display: block; border: 1px solid #767676; padding: 8px; background: #fff }

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
ul     { list-style-type: disc }
ol     { list-style-type: decimal }
b, strong { font-weight: bold }

input:not([type]), input[type=text], input[type=search], input[type=email],
input[type=url], input[type=tel], input[type=password], input[type=number], textarea {
  border: 2px solid #767676; padding: 1px 2px; background: white; color: black; font-size: 13px;
}
button, input[type=button], input[type=submit], input[type=reset] {
  border: 2px solid #767676; padding: 1px 6px; background: #e9e9ed; color: black; text-align: center; font-size: 13px;
}
select { border: 2px solid #767676; padding: 1px 2px; background: white; color: black; font-size: 13px; }
option, optgroup { display: none }

input[type=hidden] { display: none }
input:disabled, textarea:disabled, select:disabled, button:disabled,
option:disabled, optgroup:disabled, fieldset:disabled {
  color: #999999; background: #ebebe4;
}
"#;

/// Parse the UA default stylesheet.
pub(crate) fn ua_stylesheet() -> Stylesheet {
    parse_stylesheet(UA_CSS)
}
