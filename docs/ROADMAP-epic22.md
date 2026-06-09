# Roadmap — Epic 22: text & typography

Rounds out inline text layout: the line-breaking controls (`word-break`,
`overflow-wrap`, `tab-size`, `white-space: break-spaces`), text justification
(`text-align: justify`, `text-justify`, `text-indent`), line clamping
(`-webkit-line-clamp`) and hyphenation (`hyphens`). Also fixes the
`transparent` color-keyword gap found in Epic 21 (gradient stops / any color
value).

Same per-milestone agent pipeline (design → analysis → implementation → review →
verification), each landing as its own commit + push. Every feature is additive,
so pages not using it must stay byte-identical (existing tests + the golden PNG
unchanged) — except the `transparent` fix, which intentionally changes only pages
that use `transparent` in a context that previously dropped it.

Current state (reference): inline layout does greedy line breaking at soft wrap
opportunities (spaces) with `white-space` (normal/nowrap/pre/pre-wrap/pre-line),
letter/word-spacing, bidi, complex-script shaping, and `text-overflow: ellipsis`
(single line). There is **no** `word-break`/`overflow-wrap` (a long unbreakable
word overflows; it never breaks mid-word), no `tab-size` (a tab is one space), no
`white-space: break-spaces`, no `text-align: justify` (justify falls back to
left), no `text-justify`/`text-indent`, no `-webkit-line-clamp` (multi-line
clamp), and no `hyphens`. The `transparent` keyword fails to parse as a color
(it's dropped), so `linear-gradient(#000, transparent)` loses its stop.

| Milestone | Scope | Crates | Done-when | Status |
|-----------|-------|--------|-----------|--------|
| **E22-M1** | **Line-breaking controls + `transparent` fix**: `word-break` (`normal`/`break-all`/`keep-all`) — `break-all` allows a break between any two characters; `overflow-wrap`/`word-wrap` (`normal`/`break-word`/`anywhere`) — break a long word only when it would otherwise overflow the line; `tab-size` (a tab advances to the next multiple of N `ch`); `white-space: break-spaces`. Plus: fix `transparent` (and any other CSS-wide color keyword gaps) so it parses to `rgba(0,0,0,0)` everywhere a color is accepted. | `css`, `style`, `layout` | A long unbreakable word breaks mid-word under `word-break: break-all` / `overflow-wrap: break-word`; a tab advances per `tab-size`; `linear-gradient(#000, transparent)` keeps its transparent stop (tested + visual) | ✅ |
| **E22-M2** | **Justification + indent**: `text-align: justify` — distribute the extra space on a full (non-last) line across the inter-word gaps so both edges align; `text-justify` (`auto`/`inter-word`/`none` — MVP `inter-word`); `text-indent` (first-line indent, length/percentage). | `style`, `layout` | A justified paragraph has its lines stretched to both margins (last line left-aligned); `text-indent` indents the first line (tested + visual) | ✅ |
| **E22-M3** | **Line-clamp + hyphens**: `-webkit-line-clamp` (with `display: -webkit-box`/`-webkit-box-orient: vertical` or the `line-clamp` shorthand) — clamp a block to N lines, truncating the Nth with `…`; `hyphens` (`none`/`manual`/`auto` — MVP `manual`: break only at a soft hyphen `&shy;`/U+00AD, inserting a visible hyphen; `auto` documented as not breaking). | `style`, `layout` | A block clamped to 2 lines shows `…` on the 2nd; a word with a soft hyphen breaks with a hyphen at line end under `hyphens: manual` (tested + visual) | ✅ |

**Epic 22 complete.** 1237 workspace tests, clippy clean. Text & typography across
three milestones, byte-identical for unaffected pages: line-breaking controls
(word-break/overflow-wrap/tab-size/break-spaces) + the `transparent` keyword fix
(M1), justification (`text-align: justify`/`text-justify`/`text-indent`) (M2), and
`-webkit-line-clamp` + `hyphens: manual` (M3). (Note: the HTML named entity
`&shy;`→U+00AD is a separate parser gap; soft-hyphen breaking works on real U+00AD.)

## Non-goals (deferred)

- `hyphens: auto` real dictionary hyphenation (no Hunspell/pattern tables);
  language-specific (`lang`) hyphenation rules; `hyphenate-character`/`-limit-*`.
- `word-break: keep-all` full CJK/Korean rules beyond "don't break between
  letters"; `line-break` (`loose`/`strict`/`anywhere`) fine-grained class tables;
  break opportunities from the full Unicode line-breaking algorithm (UAX #14) —
  MVP breaks at spaces + (when enabled) any grapheme.
- `text-justify: inter-character`/`distribute` (CJK justification), justifying by
  stretching glyphs/letter-spacing; justification interaction with bidi runs and
  tabs precision.
- `text-indent: hanging`/`each-line`; `text-align-last`; `text-align: justify`
  for the last line.
- `-webkit-line-clamp` interaction with floats/nested blocks, `line-clamp` per the
  newer spec's `block-ellipsis`/`continue: discard`; clamping non-block content.
- `white-space-collapse`/`text-wrap: balance|pretty` (newer text-wrap shorthand),
  `text-spacing`, `word-spacing`/`letter-spacing` justification edge cases.
