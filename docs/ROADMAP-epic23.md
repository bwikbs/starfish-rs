# Roadmap — Epic 23: HTML entities & parser robustness

Hardens the HTML tokenizer + tree builder: full character-reference decoding
(named + numeric entities), the common implied/optional end-tag and auto-closing
rules so real-world markup nests correctly, and robustness for raw-text/RCDATA
elements, comments, doctype, and attribute edge cases.

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Because parsing produces the
DOM that everything else renders, changes here can shift output for malformed/
entity-bearing pages — those changes are the intended fix; well-formed pages with
no entities and proper nesting must stay byte-identical (existing tests + golden).

Current state (reference): the tokenizer/tree-builder parse tags, attributes,
text, basic comments, and a SMALL set of named entities (only the core
`&amp;`/`&lt;`/`&gt;`/`&quot;`/`&apos;` and perhaps a few more) — most named
entities (`&shy;`/`&copy;`/`&mdash;`/`&nbsp;`/…) and numeric entities (`&#169;`,
`&#x1F600;`) are NOT decoded (they pass through as literal text — confirmed by the
Epic 22 `&shy;` gap). Implicit end-tag/auto-closing rules are limited; raw-text
element handling (`<script>`/`<style>`/`<textarea>`/`<title>`) and comment/doctype
edge cases may be incomplete.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E23-M1** | **Character references**: a comprehensive named-entity table (the practical HTML5 set — `&nbsp;`/`&copy;`/`&reg;`/`&mdash;`/`&ndash;`/`&hellip;`/`&shy;`/`&times;`/`&deg;`/arrows/Greek/… a few hundred), decimal (`&#169;`) and hex (`&#xA9;`/`&#x1F600;`) numeric references (mapped to the right code point, with the C1/invalid-codepoint replacements), decoded in both text content AND attribute values; the missing-semicolon legacy forms for the common cases. | `html` | `&shy;`→U+00AD, `&copy;`→©, `&#x1F600;`→😀, `&nbsp;`→U+00A0 decode in text + attributes (tested) | ✅ |
| **E23-M2** | **Implied / optional end tags + auto-closing**: a new block-level element (or specific tags) auto-closes an open `<p>`; `<li>` closes a previous `<li>`, `<dt>`/`<dd>` each other, `<option>`/`<optgroup>`, table `<tr>`/`<td>`/`<th>`/`<thead>`/`<tbody>`/`<tfoot>`/`<colgroup>` per the common implied-close rules; implicit `<html>`/`<head>`/`<body>`; void elements never get children; stray close tags ignored. | `html` | `<p>a<p>b` → two sibling paragraphs; `<ul><li>a<li>b</ul>` → two list items; an unclosed `<td>` closes on the next cell (tested) | ✅ |
| **E23-M3** | **Raw text / comments / doctype / attributes**: `<script>`/`<style>` content as raw text (no tag/entity parsing inside), `<textarea>`/`<title>` as RCDATA (entities decoded, no tags), comment edge cases (`<!-- -->`, bogus comments), doctype tolerance (`<!DOCTYPE html>` + legacy), attribute edge cases (unquoted values, duplicate attributes keep the first, attributes after `/`, missing-value boolean attributes), and self-closing on void vs non-void. | `html` | `<script>if(a<b){}</script>` keeps its body verbatim; `<textarea>&amp;</textarea>` shows `&`; a duplicate attribute keeps the first; a bogus comment doesn't break parsing (tested) | ☐ |

## Non-goals (deferred)

- The full HTML5 tree-construction state machine (all insertion modes), the
  adoption agency algorithm for misnested formatting elements, and active
  formatting-element reconstruction beyond the common cases.
- Full foster-parenting for misplaced table content; `<template>` contents;
  fragment-parsing context elements; speculative parsing.
- The complete 2000+ named-entity table (we ship the practical few-hundred set);
  named references without a semicolon beyond the common legacy set.
- `document.write` re-entrancy, scripting-driven parser pauses, encoding sniffing
  / `<meta charset>` re-decode, BOM handling.
- SVG/MathML foreign-content integration-point subtleties beyond the existing
  inline-SVG support (Epic 9).
