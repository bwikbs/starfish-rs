# M5 — Paint design note

Scope: the `starfish-paint` crate + the `starfish-cli` binary. Given the M4
box tree (`LayoutBox`) and the M3 `StyledTree`, **rasterize** the page to an
RGBA pixmap with [`tiny-skia`] and write a **PNG**. Provide **real text
rendering** with a vendored TrueType font so layout's `TextMeasurer` uses REAL
glyph advances/metrics (replacing M4's `DefaultMeasurer`). Wire the full
pipeline behind a CLI: `starfish render <input.html> -o <out.png> [--width N]`.

This is the final milestone of "render a static HTML page to PNG". No JS, no
networking, no incremental reflow.

Guiding rule (project "Simplicity First"): the **minimum correct** painter — a
flat display list of fill-rects and glyph runs, src-over compositing, no
transform stack, no clipping. Every type below has a consumer in painting or
the CLI. See §8 for explicit non-goals.

---

## 0. Inputs M5 actually receives (recap of the real M4/M3 API)

Pinned to the exact types, not an idealized API.

From `starfish-layout` (`crates/layout/src/…`, re-exported from `lib.rs`):

- `pub fn layout(doc: &Document, styled: &StyledTree, viewport_width: f32,
  measurer: &dyn TextMeasurer) -> LayoutBox` — the entry. **M5 passes its own
  font-backed measurer here** (not `DefaultMeasurer`).
- `LayoutBox` (fields are public): `kind: BoxKind`, `style: BoxStyleRef`,
  `text: Option<String>`, `dimensions: Dimensions`, `children: Vec<LayoutBox>`.
  Accessors: `kind()`, `children() -> &[LayoutBox]`, `dimensions() ->
  &Dimensions`, `text() -> Option<&str>`, `style(&StyledTree) ->
  Option<&ComputedStyle>`, `walk(&mut dyn FnMut(&LayoutBox))`.
- `BoxKind::{BlockContainer, InlineBox, AnonymousBlock, TextRun, LineBox}`.
- `BoxStyleRef::{Node(NodeId), Anonymous(NodeId)}`, with `.node() -> NodeId`.
- `Dimensions { content: Rect, padding/border/margin: EdgeSizes }` with
  `padding_box()`, `border_box()`, `margin_box() -> Rect`. `Rect { x, y, width,
  height }` in **absolute page space** (root content origin at `(0,0)`; no
  transforms).
- `TextMeasurer` trait: `measure(&self, text: &str, font_size: f32, weight:
  FontWeight) -> f32` and `line_metrics(&self, font_size: f32) -> LineMetrics
  { ascent, descent }` (default 0.8/0.2 × font_size).
- Re-exports: `ComputedStyle`, `FontWeight`, `Document`, `NodeId`. M5 also uses
  `starfish_style::StyledTree`, `style_tree`, and `Rgba`.

From `starfish-style` (`crates/style/src/computed.rs`):

- `ComputedStyle` fields M5 reads: `color: Rgba`, `background_color: Rgba`,
  `border_{top,right,bottom,left}_width: f32`, `border_style: BorderStyle`,
  `border_color: Rgba`, `font_size: f32`, `font_weight: FontWeight`. (Geometry
  already baked into `Dimensions` by M4; M5 reads style only for paint.)
- `BorderStyle::{None, Solid}` — only `Solid` is painted (M2/M3 already fold
  dashed/dotted → `Solid`).
- `Rgba { r, g, b, a: u8 }`. `background_color` initial is **transparent**
  (`a == 0`); `color` initial is opaque black.
- `StyledTree::get(id) -> Option<&ComputedStyle>` — **`None` for anonymous
  boxes and text nodes**. `LayoutBox::style(styled)` calls this with
  `BoxStyleRef::node()`. For `TextRun`, the ref is `Node(parent_element_id)`,
  so `style()` returns the parent element's style (the font/color source). For
  `AnonymousBlock`/`LineBox`, the ref is the container element; `style()` may
  still resolve, but anonymous boxes have transparent background and no border,
  so painting them is a no-op anyway (§3.4).

From `starfish-html` (`crates/html/src/lib.rs`): `pub fn parse(html: &str) ->
Document`. **`<style>` element content is inserted as ordinary `Text` children**
under the `<style>` element (the M1 tree builder treats head children as
ordinary markup — see `in_head`/`insert_text`). So extracting author CSS =
concatenating the text of every `<style>` element's `Text` descendants (§5.1).

From `starfish-css`: `pub fn parse_stylesheet(css: &str) -> Stylesheet`.

---

## 1. Third-party crates

Added to `crates/paint/Cargo.toml`. `Cargo.lock` is committed, so versions are
pinned there; the manifest uses caret requirements at the versions current as of
2026-06. Two new third-party crates only (plus the existing workspace crates).

```toml
[dependencies]
starfish-dom    = { workspace = true }
starfish-html   = { workspace = true }
starfish-css    = { workspace = true }
starfish-style  = { workspace = true }
starfish-layout = { workspace = true }

tiny-skia = "0.11"   # CPU rasterizer: Pixmap, Paint, Rect, fill + PNG output
fontdue   = "0.9"    # pure-Rust TTF: glyph metrics/advances + coverage rasterize
```

`crates/cli/Cargo.toml` adds only `starfish-paint = { workspace = true }` (the
CLI is thin — argument parsing is hand-rolled, no `clap`, see §6). The workspace
`Cargo.toml` already lists `starfish-paint`; add the missing `starfish-html`,
`starfish-css`, `starfish-style`, `starfish-layout` lines under
`[workspace.dependencies]` if not already present (M4 added layout; verify).

### 1.1 Rationale

- **`tiny-skia`** — the project's chosen raster backend (ROADMAP §"Third-party
  choices"). Pure-Rust (no system Skia/cairo), gives `Pixmap` (RGBA8,
  premultiplied internally), `Paint` + `Rect` + `PathBuilder` for filled
  rectangles, and `Pixmap::encode_png()` (via the bundled `png` crate) so we
  need **no separate `png` dependency**. We use rect fills (`fill_rect`) for
  backgrounds/borders and write glyph coverage directly into the pixmap's
  `data_mut()` byte buffer (manual src-over, §4.3) — simpler and faster than
  building a path per glyph.

- **`fontdue`** — pure-Rust, no `unsafe`-FFI, and exposes exactly the two things
  M5 needs from **one** crate:
  - **Metrics for measuring**: `Font::metrics(ch, px)` returns `Metrics {
    advance_width, width, height, xmin, ymin, .. }`; `Font::horizontal_line_metrics(px)`
    returns `LineMetrics { ascent, descent, line_gap, new_line_size }`. This
    backs the layout `TextMeasurer` (§2, §5) so **measuring == painting**.
  - **Rasterizing for painting**: `Font::rasterize(ch, px) -> (Metrics,
    Vec<u8>)` where the `Vec<u8>` is an 8-bit **coverage** bitmap (alpha mask)
    sized `metrics.width × metrics.height`, with placement offsets `xmin`
    (left bearing) and `ymin` (bottom offset from baseline). Coverage × text
    color = the painted pixels.
  Choosing `fontdue` over `ab_glyph`/`rusttype`: one crate covers both metrics
  and rasterization with a tiny API; `ab_glyph` needs a separate outline→mask
  step and `rusttype` is effectively deprecated. `cosmic-text` is full shaping
  (overkill — we do no shaping, no kerning, no fallback). Per Simplicity First,
  `fontdue` is the minimal fit.

---

## 2. Font module (`crates/paint/src/font.rs`)

Loads the vendored regular + bold faces and exposes advance/metrics/rasterize.
**Backs the layout `TextMeasurer` trait** so the same metrics drive line
breaking and painting.

### 2.1 Vendored assets

Copy the two free DejaVu faces into the crate and embed them so output is
deterministic and the binary is self-contained (no runtime font discovery):

```
crates/paint/assets/DejaVuSans.ttf          (from /usr/share/fonts/truetype/dejavu/)
crates/paint/assets/DejaVuSans-Bold.ttf
crates/paint/assets/LICENSE-DejaVu.txt       (the Bitstream Vera + DejaVu notice)
```

Embed with `include_bytes!` (no I/O, deterministic):

```rust
const REGULAR_TTF: &[u8] = include_bytes!("../assets/DejaVuSans.ttf");
const BOLD_TTF:    &[u8] = include_bytes!("../assets/DejaVuSans-Bold.ttf");
```

### 2.2 Types & API

```rust
use fontdue::Font as FontdueFont;
use starfish_style::FontWeight;
use starfish_layout::{LineMetrics, TextMeasurer};

/// One rasterized glyph: an 8-bit coverage mask + its placement relative to the
/// pen origin (baseline). `left`/`top` are the offsets from the pen position to
/// the top-left of the mask, in device pixels (already rounded by the caller).
pub struct GlyphBitmap {
    pub width: usize,      // mask columns  (metrics.width)
    pub height: usize,     // mask rows     (metrics.height)
    pub left: i32,         // metrics.xmin  (left side bearing, +x = right)
    pub top: i32,          // distance from baseline up to mask top (= -(ymin+height)) — see §4.2
    pub advance: f32,      // metrics.advance_width
    pub coverage: Vec<u8>, // width*height, 0..=255 alpha
}

/// The two-face font database. Cheap to clone-ref; built once per render.
pub struct FontDb {
    regular: FontdueFont,
    bold: FontdueFont,
}

impl FontDb {
    /// Load the embedded DejaVu faces. Infallible in practice (assets are
    /// validated at build time); returns Result to surface a corrupt-asset bug
    /// without panicking.
    pub fn load() -> Result<FontDb, String>;

    /// Pick the face for a weight: weight >= 600 → bold, else regular.
    fn face(&self, weight: FontWeight) -> &FontdueFont {
        if weight.0 >= 600 { &self.bold } else { &self.regular }
    }

    /// Sum of per-glyph advance widths of `text` at `font_size` px. No kerning
    /// (sum of advances). Missing glyphs fall back to fontdue's .notdef advance.
    pub fn advance_width(&self, text: &str, font_size: f32, weight: FontWeight) -> f32 {
        let f = self.face(weight);
        text.chars()
            .map(|c| f.metrics(c, font_size).advance_width)
            .sum()
    }

    /// Ascent/descent (positive down) for a line at `font_size`. From
    /// fontdue's horizontal_line_metrics; used for baseline placement (§4.2).
    pub fn line_metrics(&self, font_size: f32, weight: FontWeight) -> LineMetrics {
        let lm = self.face(weight)
            .horizontal_line_metrics(font_size)
            .expect("scalable face has horizontal metrics");
        LineMetrics { ascent: lm.ascent, descent: -lm.descent } // fontdue descent is negative
    }

    /// Rasterize one char to a coverage mask + placement. Whitespace yields an
    /// empty mask (width==0) but a real advance.
    pub fn rasterize_glyph(&self, ch: char, font_size: f32, weight: FontWeight) -> GlyphBitmap;
}
```

`FontWeight` selects the face by **weight ≥ 600 → bold**, matching the common
`bold`==700 / `normal`==400 split (M3 normalizes `bold`→700). DejaVu has no
synthetic intermediate weights, so this is a clean two-way switch.

### 2.3 Backing the layout `TextMeasurer`

A thin newtype adapts `FontDb` to the layout trait so `layout()` can be driven
by real metrics:

```rust
/// Wraps a `FontDb` (by ref) to satisfy the layout `TextMeasurer` trait, so
/// line-breaking during layout uses the same advances the painter draws.
pub struct FontMeasurer<'a>(pub &'a FontDb);

impl<'a> TextMeasurer for FontMeasurer<'a> {
    fn measure(&self, text: &str, font_size: f32, weight: FontWeight) -> f32 {
        self.0.advance_width(text, font_size, weight)
    }
    fn line_metrics(&self, font_size: f32) -> LineMetrics {
        // weight-independent line box; use the regular face.
        self.0.line_metrics(font_size, FontWeight(400))
    }
}
```

Kerning is **out**: `measure` is a plain sum of advances, so the painter
positioning each glyph by the same advances yields pixel-consistent runs.

---

## 3. Painting model — the display list

We do **not** paint straight from the tree. We first walk the `LayoutBox` tree
to build a flat `Vec<PaintCmd>` in correct paint order, then rasterize the list
(§4). The display list makes order explicit and is trivially unit-testable
(§9) without a pixmap.

### 3.1 Commands

```rust
/// Device-space (page-space) paint command. Coordinates are f32 page pixels;
/// the rasterizer rounds. Colors are straight (non-premultiplied) Rgba.
pub enum PaintCmd {
    /// Filled rectangle (a background, or one border edge). Skipped upstream if
    /// the color is fully transparent or the rect is empty.
    FillRect { rect: Rect, color: Rgba },
    /// A run of text drawn with its top-left content origin at `origin`, on the
    /// baseline derived from `font_size`/ascent (§4.2).
    GlyphRun {
        origin: (f32, f32),   // content rect (x, y) of the TextRun
        text: String,
        font_size: f32,
        weight: FontWeight,
        color: Rgba,
        ascent: f32,          // baseline offset from `origin.1` (top), from FontDb
    },
}
```

`Rect` is reused from `starfish-layout` (re-exported). Borders are emitted as up
to four `FillRect`s (one per non-zero edge), so the rasterizer handles a single
primitive plus glyph runs.

### 3.2 Build order (the tree walk)

`fn build_display_list(root: &LayoutBox, styled: &StyledTree, fonts: &FontDb) ->
Vec<PaintCmd>`. **Pre-order**, parent emitted before children (so children paint
on top), and **within one box**: background → borders → (recurse into children).
Text is emitted when a `TextRun` box is reached (its own node in the walk), so a
`TextRun` naturally paints after its containing block's background/border
because the block is an ancestor visited earlier. This gives the required global
order: backgrounds/borders of ancestors first, text last among siblings stacked
correctly.

```rust
fn build_display_list(root, styled, fonts) -> Vec<PaintCmd> {
    let mut out = Vec::new();
    paint_box(root, styled, fonts, &mut out);
    out
}

fn paint_box(b, styled, fonts, out) {
    match b.kind() {
        BoxKind::TextRun => emit_text(b, styled, fonts, out),   // §3.5
        _ => {
            emit_background(b, styled, out);                    // §3.3
            emit_borders(b, styled, out);                       // §3.4
        }
    }
    for child in b.children() {
        paint_box(child, styled, fonts, out);
    }
}
```

A `TextRun` has no children and no box decorations, so it only emits a glyph
run. `LineBox`/`AnonymousBlock` resolve to transparent background + no border,
so their `emit_background`/`emit_borders` are no-ops.

### 3.3 Background

Read the box's style via `b.style(styled)`. If `None` (anonymous/unstyled) or
`bg.a == 0` (transparent) → emit nothing. Otherwise emit one `FillRect` over the
**border-box** area (CSS `background-clip: border-box` default):

```rust
let Some(style) = b.style(styled) else { return };
let bg = style.background_color;
if bg.a == 0 { return; }
out.push(PaintCmd::FillRect { rect: b.dimensions().border_box(), color: bg });
```

(Backgrounds are only meaningful on `BlockContainer`/`InlineBox`; for `InlineBox`
the M4 geometry is a bounding-rect approximation (M4 §4.7) — good enough.)

### 3.4 Borders

Only `BorderStyle::Solid` with a non-zero width per side is painted; `None` or
zero width → skip that edge. Four edges are filled rects laid between the
padding-box and the border-box:

```rust
let Some(style) = b.style(styled) else { return };
if style.border_style != BorderStyle::Solid { return; }
let bc = style.border_color;
if bc.a == 0 { return; }
let bb = b.dimensions().border_box();   // outer
let d  = b.dimensions();
// top
if d.border.top > 0.0 {
    out.push(FillRect { rect: Rect { x: bb.x, y: bb.y, width: bb.width, height: d.border.top }, color: bc });
}
// bottom
if d.border.bottom > 0.0 {
    out.push(FillRect { rect: Rect { x: bb.x, y: bb.y + bb.height - d.border.bottom,
                                     width: bb.width, height: d.border.bottom }, color: bc });
}
// left  (between top and bottom edges to avoid double-painting corners)
if d.border.left > 0.0 {
    out.push(FillRect { rect: Rect { x: bb.x, y: bb.y + d.border.top,
                                     width: d.border.left,
                                     height: bb.height - d.border.top - d.border.bottom }, color: bc });
}
// right
if d.border.right > 0.0 {
    out.push(FillRect { rect: Rect { x: bb.x + bb.width - d.border.right, y: bb.y + d.border.top,
                                     width: d.border.right,
                                     height: bb.height - d.border.top - d.border.bottom }, color: bc });
}
```

All four edges share `border_color` (M3 stores one color/style for the box,
matching the `ComputedStyle` shape). Corners go to the horizontal edges (full
width top/bottom; left/right inset vertically) — simple, correct for uniform
solid borders.

### 3.5 Text run

A `TextRun`'s style ref is its parent element (`Node(parent_id)`), so
`b.style(styled)` gives `color`, `font_size`, `font_weight`:

```rust
let style = b.style(styled).unwrap_or(&INITIAL);  // robustness fallback
let text  = b.text().unwrap_or("");
let c = b.dimensions().content;                    // absolute Rect
let lm = fonts.line_metrics(style.font_size, style.font_weight);
out.push(PaintCmd::GlyphRun {
    origin: (c.x, c.y),
    text: text.to_string(),
    font_size: style.font_size,
    weight: style.font_weight,
    color: style.color,
    ascent: lm.ascent,
});
```

Baseline = `c.y + ascent` (top of content rect down by ascent). M4 sets the
`TextRun`'s `content.height = used line-height`; we top-align the glyphs' ascent
within that, which matches M4's top-alignment approximation (M4 §4.4).

---

## 4. Rasterization (`crates/paint/src/raster.rs`)

`fn rasterize(cmds: &[PaintCmd], width: u32, height: u32, fonts: &FontDb) ->
Pixmap`.

### 4.1 Canvas + dimensions

- `let mut pixmap = Pixmap::new(width, height).expect("nonzero dims");`
- Fill the whole canvas **white** (opaque `#ffffff`) first — the page's default
  backdrop — via `pixmap.fill(tiny_skia::Color::WHITE)`.
- **Pixel dimensions** (computed in `paint`, §7):
  - `width  = round(viewport_width).max(1)`.
  - `height = round(root.dimensions().margin_box().height).max(1)` — the page
    grows with content (M4 §5). Using the **margin-box** captures the root's
    bottom margin; `.max(1)` guards the empty-document case.

### 4.2 Fill rects

For each `FillRect`, build a `tiny_skia::Rect` and `fill_rect` with a non-AA
`Paint` whose color is the straight Rgba converted to tiny-skia's premultiplied
`Color`:

```rust
let mut paint = Paint::default();
paint.anti_alias = false;     // crisp box edges; matches integer layout
paint.set_color_rgba8(color.r, color.g, color.b, color.a);  // tiny-skia premultiplies
if let Some(r) = Rect::from_xywh(rect.x, rect.y, rect.width.max(0.0), rect.height.max(0.0)) {
    pixmap.fill_rect(r, &paint, Transform::identity(), None);
}
```

`set_color_rgba8` takes straight (non-premultiplied) bytes and tiny-skia does
src-over compositing with the existing pixels, so semi-transparent backgrounds
composite over white correctly. Empty/degenerate rects (`from_xywh` returns
`None`) are skipped.

### 4.3 Glyph runs (manual coverage blend)

tiny-skia has no text API, so we rasterize each glyph with `fontdue` and blend
its coverage mask directly into the pixmap buffer with src-over. Pen advances
left→right; positions are **subpixel-rounded** to integers.

```rust
fn draw_glyph_run(pixmap, run, fonts) {
    let (ox, baseline) = (run.origin.0, run.origin.1 + run.ascent);
    let mut pen_x = ox;
    let face_weight = run.weight;
    for ch in run.text.chars() {
        let g = fonts.rasterize_glyph(ch, run.font_size, face_weight);
        if g.width > 0 && g.height > 0 {
            // top-left of the mask in device space:
            let gx = (pen_x + g.left as f32).round() as i32;
            // baseline up by the glyph's top extent:
            let gy = (baseline - g.top as f32).round() as i32;  // g.top = ascent of mask above baseline
            blit_coverage(pixmap, &g, gx, gy, run.color);
        }
        pen_x += g.advance;
    }
}
```

`g.top` is computed in `rasterize_glyph` from fontdue's `ymin`/`height`:
fontdue's `ymin` is the offset of the **bottom** of the mask from the baseline
(up positive). The mask's top sits `ymin + height` above the baseline, so we
store `top = ymin + height` and place the mask top at `baseline - top`.

`blit_coverage` does per-pixel src-over of `run.color` weighted by coverage,
clipping to the pixmap bounds:

```rust
fn blit_coverage(pixmap, g, gx, gy, color) {
    let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
    let buf = pixmap.data_mut();         // &mut [u8], RGBA premultiplied, row-major
    for row in 0..g.height as i32 {
        let py = gy + row;
        if py < 0 || py >= ph { continue; }
        for col in 0..g.width as i32 {
            let px = gx + col;
            if px < 0 || px >= pw { continue; }
            let cov = g.coverage[(row as usize)*g.width + col as usize];
            if cov == 0 { continue; }
            let a = (cov as u32 * color.a as u32) / 255;   // glyph alpha × text alpha
            src_over_pixel(buf, (py*pw + px) as usize * 4, color, a as u8);
        }
    }
}
```

`src_over_pixel` composites a straight-color source with alpha `a` over the
destination premultiplied pixel: `dst = src*a + dst*(1-a)` per channel (the
pixmap stores premultiplied RGBA, so we premultiply the source by `a` and the
destination is already premultiplied). Since glyphs draw last over opaque
backgrounds, this is the standard premultiplied src-over. Tiny helper, ~10
lines, unit-tested on a 1×1 blend.

### 4.4 PNG output

`pixmap.encode_png() -> Result<Vec<u8>, _>` (tiny-skia bundles `png`), written
to the output path; or `pixmap.save_png(path)` directly. The public `paint`
returns the `Pixmap`; the CLI calls `save_png` (§6).

---

## 5. Pipeline & TextMeasurer integration

The whole point of M5's font module is that **layout runs with the same
measurer the painter draws with**, so line breaks land where glyphs land.

### 5.1 CSS extraction from the DOM

M5's only CSS source is **inline `<style>` blocks** (no networking, no linked
`<link rel=stylesheet>` — noted §8). Because the M1 tree builder stores
`<style>` content as `Text` children, we collect them:

```rust
/// Concatenate the text of every <style> element's Text descendants, in
/// document order.
fn extract_css(doc: &Document) -> String {
    let mut css = String::new();
    let mut stack = vec![doc.root()];
    // simple DFS; collect under any <style>
    walk(doc, doc.root(), &mut |id| {
        if doc.tag_name(id) == Some("style") {
            for c in doc.children(id) {
                if let NodeKind::Text(t) = doc.kind(c) { css.push_str(t); css.push('\n'); }
            }
        }
    });
    css
}
```

Caveat: the M1 tokenizer parses `<style>` content as ordinary markup (no
RAWTEXT), so CSS containing `<` could be mis-tokenized. Real-world author CSS
rarely contains `<`; documented as a known limitation (§8).

### 5.2 Flow

```
html: &str
  └─ starfish_html::parse(html)            → Document
  └─ extract_css(&doc)                       → String (concatenated <style> text)
  └─ starfish_css::parse_stylesheet(&css)    → Stylesheet
  └─ starfish_style::style_tree(&doc, &[sheet]) → StyledTree
  └─ FontDb::load()                          → FontDb
  └─ layout(&doc, &styled, viewport_width, &FontMeasurer(&fonts)) → LayoutBox  (real metrics!)
  └─ width  = round(viewport_width).max(1)
     height = round(root.dimensions().margin_box().height).max(1)
  └─ build_display_list(&root, &styled, &fonts) → Vec<PaintCmd>
  └─ rasterize(&cmds, width, height, &fonts)    → Pixmap
```

`layout` is invoked with `&FontMeasurer(&fonts)` (not `DefaultMeasurer`), so
wrapping and line-box heights reflect real DejaVu advances and ascent/descent.

---

## 6. CLI (`crates/cli/src/main.rs`)

```
starfish render <input.html> -o <out.png> [--width N]
```

- Subcommand `render` (only one for now). `<input.html>` positional.
- `-o <path>` / `--output <path>` (required): PNG output path.
- `--width N` (optional, default **800**): viewport width in CSS px (`u32`).

**Hand-rolled parser** (no `clap`, per Simplicity First — the grammar is tiny):
iterate `std::env::args()`, match flags, collect the positional. On any parse
problem, print usage to stderr and exit non-zero.

```rust
fn main() {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => {}
        Err(e) => { eprintln!("starfish: {e}"); std::process::exit(1); }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    // parse: require args[0]=="render", an input path, -o/--output, optional --width
    // read input file (map io error → friendly String)
    // let pixmap = starfish_paint::render_html(&html, width as f32);
    // pixmap.save_png(out).map_err(|e| format!("writing {out}: {e}"))?;
    // println!("wrote {out} ({}x{})", pixmap.width(), pixmap.height());
}
```

Behavior:
- Reads the input file (`std::fs::read_to_string`); friendly error on missing
  file / non-UTF8.
- Runs `starfish_paint::render_html` (§7), writes PNG, prints
  `wrote out.png (WxH)`.
- **Never panics on bad input**: malformed HTML/CSS already recover (M1/M2 are
  lenient); empty document → 1×N white pixmap; missing args/file → friendly
  stderr message + exit code 1.

`crates/cli/Cargo.toml` depends only on `starfish-paint` (the pipeline lives in
the crate; the CLI is plumbing).

---

## 7. Public API of `starfish-paint` (`crates/paint/src/lib.rs`)

Keep the CLI thin: the crate owns the pipeline and exposes two entry points plus
the font/measurer/display-list types.

```rust
mod font;     // FontDb, FontMeasurer, GlyphBitmap
mod display;  // PaintCmd, build_display_list
mod raster;   // rasterize, blit

pub use font::{FontDb, FontMeasurer, GlyphBitmap};
pub use display::PaintCmd;
pub use tiny_skia::Pixmap;                       // re-export so callers needn't depend on tiny-skia
pub use starfish_layout::{LayoutBox, Rect};
pub use starfish_style::StyledTree;

/// End-to-end: HTML string → rendered RGBA pixmap. Parses HTML, extracts inline
/// <style> CSS, styles, lays out with the font-backed measurer, paints. The
/// page height grows with content; the pixmap is `round(viewport_width) ×
/// round(root margin-box height)` (each min 1).
pub fn render_html(html: &str, viewport_width: f32) -> Pixmap;

/// Paint an already-laid-out box tree to a pixmap of the given device size.
/// `width`/`height` are device pixels (the caller decides; `render_html`
/// derives them from the root box). Builds the display list and rasterizes.
pub fn paint(
    layout_root: &LayoutBox,
    styled: &StyledTree,
    fonts: &FontDb,
    width: u32,
    height: u32,
) -> Pixmap;
```

`render_html` builds a `FontDb` once, calls `layout(.., &FontMeasurer(&fonts))`,
computes the dimensions, then delegates to `paint`. `paint` =
`build_display_list` + `rasterize`. Exposing `paint` separately keeps the layout
step injectable and makes the display list testable independently of the parse
pipeline.

---

## 8. Edge cases & non-goals

Handled:
- Transparent background (`a == 0`) → not painted (white canvas shows through).
- `BorderStyle::None` or zero-width edge → that edge skipped.
- Empty document → root box of (near) zero height → `height.max(1)` → a 1-px
  white strip, no panic.
- Glyphs partially/fully outside the pixmap → clipped per-pixel in `blit`.
- Whitespace / missing glyphs → empty mask but real advance (no gap collapse).
- Semi-transparent backgrounds → src-over composite over white via tiny-skia.

Explicit **non-goals** (documented limitations):
- **No `<img>` / images / `background-image`** — `url(...)` ignored.
- **No gradients, no `border-radius`, no `box-shadow`, no `opacity`** (beyond
  per-color alpha), **no blend modes** beyond src-over.
- **No clipping / `overflow`** — content draws outside its box (matches M4's
  unclamped overflow).
- **No subpixel/hinted text positioning** beyond integer-rounded coverage
  blitting; **no kerning** (sum of advances), **no shaping/ligatures/bidi**, no
  font fallback beyond DejaVu's own `.notdef`.
- **No `@font-face` / web fonts**; the two embedded DejaVu faces only.
- **No linked CSS** (`<link rel=stylesheet>`) and **no networking** — inline
  `<style>` blocks only. CSS containing `<` may mis-tokenize (M1 has no RAWTEXT
  for `<style>`).
- **Non-uniform per-side border colors/styles not supported** — M3 stores one
  color + one style per box (matches `ComputedStyle`).
- **Borders are flat fills**; mitered corners go to the horizontal edges.

---

## 9. Test plan

Unit tests live in `crates/paint/src/…`; the golden CLI test in
`crates/cli/tests/` (or `crates/paint/tests/`). A small fixed HTML+CSS fixture
is saved under `crates/paint/tests/fixtures/` (or `docs/`).

### 9.1 Font module (`font.rs`)
- `advance_width` sanity: a wider string advances more than a shorter one;
  `advance_width("", ..) == 0`; bold "M" advance > 0; advance scales (roughly)
  with `font_size`. Avoid asserting exact px (font-version dependent) — assert
  monotonicity / ratios.
- `face()` selection: `weight 700 → bold`, `400 → regular`, boundary `600 →
  bold`, `599 → regular`.
- `line_metrics`: `ascent > 0`, `descent > 0`, `ascent + descent` ≈ within a
  sane band of `font_size` (e.g. `0.9*fs .. 1.5*fs`).
- `rasterize_glyph('x', ..)` returns a non-empty mask with some `coverage > 0`;
  `rasterize_glyph(' ', ..)` returns `width == 0` but `advance > 0`.

### 9.2 Display list ordering (`display.rs`) — no pixmap needed
- A `<div style=bg+border><p>text</p></div>`: assert the emitted `Vec<PaintCmd>`
  has the div's `FillRect` (background) **before** its border `FillRect`s, and
  **all of those before** the `GlyphRun` for the text (parent-before-child,
  bg→border→text).
- Two sibling divs: parent's commands precede each child's; first sibling's
  commands precede the second's (document order).
- A transparent-background, no-border box emits **no** `FillRect` (only its
  children's commands).
- A `TextRun` emits exactly one `GlyphRun` with the parent element's `color`
  and `font_size`.

### 9.3 Rasterization dimensions
- `render_html(small_html, 800.0)`: `pixmap.width() == 800`,
  `pixmap.height() == round(root margin-box height)` (assert > 0 and equals the
  layout-derived value).
- Empty/whitespace HTML → `height >= 1`, no panic.

### 9.4 Pixel sampling (font-independent primitives)
- A `body{margin:0} div{width:100px;height:50px;background:#ff0000}`: sample a
  pixel at e.g. `(10, 10)` → opaque red `(255,0,0,255)`; a pixel well outside
  the div (e.g. `(400, 400)` if canvas tall enough, else `(150,10)`) → white.
- A solid border: `div{width:100px;height:50px;border:5px solid #0000ff}` on a
  transparent background → sample a pixel inside the top border band (e.g.
  `(50, 2)`) → blue; a pixel in the interior → white.
- These assertions use only fill-rects → **font-version independent**.

### 9.5 Text presence (region, not exact pixels)
- `body{margin:0} p{color:#000;font-size:20px}` with text "Hello": scan the
  text's content-rect region and assert **at least one non-white pixel** exists
  (glyphs drew) and that pixels far from any text remain white. Do **not**
  assert exact glyph pixels (font-version brittle).

### 9.6 Golden test (CLI / pipeline)
- Fixture: a tiny fixed HTML+CSS (a colored bordered box + a line of text) at
  `--width 200`, saved under `tests/fixtures/page.html`.
- Render to a `Pixmap`; assert:
  - dimensions: `width == 200`, `height ==` the layout-derived value (stable
    across font versions only if the height comes from explicit box heights —
    prefer explicit `height`/`px` boxes in the fixture so the golden height is
    deterministic; if text-height-driven, assert a tolerance band).
  - a handful of **sampled pixels** for the background and border (font
    independent) — exact RGBA.
  - text region has non-white pixels.
- **Avoid full-image PNG goldens** — they break on any DejaVu/fontdue update.
  Sample specific font-independent pixels instead.

---

## 10. Module layout & implementation order

`crates/paint/src/`:
```
lib.rs      // render_html, paint, re-exports, extract_css, pipeline wiring, tests
font.rs     // FontDb, FontMeasurer, GlyphBitmap, embedded TTFs, advance/metrics/raster
display.rs  // PaintCmd, build_display_list (bg/border/text emission), order tests
raster.rs   // Pixmap setup, fill_rect, glyph coverage blit (src-over), PNG
```
`crates/paint/assets/` — the two `.ttf` files + `LICENSE-DejaVu.txt`.
`crates/cli/src/main.rs` — hand-rolled arg parse + `render_html` + `save_png`.

Implementation order (each step verifiable):
1. **Cargo manifests**: add `tiny-skia`, `fontdue` to paint; `starfish-paint`
   to cli; vendor the fonts + license. `cargo build` succeeds.
2. **`font.rs`**: load embedded faces, `advance_width`/`line_metrics`/
   `rasterize_glyph`, `face()` switch, `FontMeasurer`. Tests §9.1.
3. **`display.rs`**: `PaintCmd`, `build_display_list`. Tests §9.2.
4. **`raster.rs`**: white canvas, `fill_rect`, coverage blit, src-over. Unit-test
   a 1×1 blend.
5. **`lib.rs`**: `extract_css`, `render_html`, `paint`, re-exports. Tests
   §9.3–§9.5.
6. **`cli/main.rs`**: hand-rolled `render` parser, file I/O, `save_png`, friendly
   errors. Smoke: run against the fixture.
7. **Golden test** §9.6 under `tests/`.

[`tiny-skia`]: https://docs.rs/tiny-skia
