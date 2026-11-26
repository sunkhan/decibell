# decibell-text

The text engine of the GPUI client: every label, message body and (later) input is laid out and
painted by this crate, on top of GPUI's shaper and glyph atlas. GPUI's own text elements cannot
draw a chat client that looks like its Chromium original (no letter-spacing, no inline images, no
mixed sizes in a paragraph, no selection, no slanted italics, line breaking that differs), so the
engine does its own paragraph layout, the way Chromium's LayoutNG does it, and checks itself
against Chromium line by line.

It knows no theme: fonts, sizes and colours arrive in style structs. It depends on `gpui` and the
three content crates (`decibell-richtext`, `decibell-highlight`, `decibell-tex`), never on
`decibell-ui` or `decibell-model`. View code normally reaches it through `decibell_ui::Label`;
feature code that shows messages uses `RichText` directly.

## Concepts

| Type | What it is |
|---|---|
| `Engine` | One per app (`Engine::global(cx)`): resolved faces, the layout caches, the emoji and formula stores, and the only caller of GPUI's shaper. Tests build their own (`Engine::new(text_system)`). |
| `FontSpec` → `Face` | A family, a CSS weight, italic or not, resolved once to a GPUI font. An italic that has no face is slanted (0.25, Skia's synthetic italic); a weight ≥ 600 on a family without one is emboldened. `Face` is `Copy`: keep it in your styles. |
| `TextStyle` | One run's style: face, size, colour, `LineHeight` (a CSS factor or px), letter-spacing in px, underline / strike, an optional `Chip` (inline code box), a start margin, selectable or not. |
| `Wrapping` | `white-space` + `word-break` + `overflow-wrap` + `tab-size` + line clamp + ellipsis. Constants for what the client uses: `NORMAL`, `NOWRAP`, `TRUNCATE`, `MESSAGE` (the message body: `pre-wrap` + `break-all` + `anywhere`), `PRE_WRAP`, `PRE`; `.clamp(n)`. |
| `Paragraph` | Inline content **shaped once** (`ParagraphBuilder`: `.text(str, &style, tag)`, `.inline_box(w, h, below_baseline, tag)`), independent of width. Immutable, `Rc`. |
| `Layout` | A paragraph wrapped at a width: lines, fragments, chip and decoration rectangles, exact `width()` / `height()` / baselines. It points at the paragraph's glyphs and owns none, so another width costs one pass over the text and no shaping. Immutable, `Rc`; `paint()` allocates nothing. |
| `Pos` | A caret position: a byte offset into `Paragraph::text()` (an inline box is one U+FFFC) and an `Affinity` for the end of a wrapped line. |
| `RichLayout` | A whole message body (`decibell-richtext` `Document`) at a width: paragraphs as `Layout`s with emoji and inline formulas as boxes, and a `Slot` (a reserved rectangle) per fenced code block and display formula. `RichPos` = (block, offset). |
| `Label`, `TextBlock`, `MathBlock`, `RichText` | The elements. |

### Positions and the selection contract

- `layout.hit(x, y)` → `Hit { pos, cluster, tag, line }`: the nearest caret position for any point
  (also outside the text, for drags), the cluster under the point, and the **tag** of the run or
  box there (the `tag` you passed to the builder: a span index, a link id).
- `layout.caret(pos)`, `rects_for_range(from, to, |rect| …)` (one line-box-high rectangle per
  line), `word_at(offset)` (double click), `line_range(pos)`, `line_of(pos)`.
- `paragraph.locate(offset)` → (tag, offset inside that run's text) and `offset_of(tag, local)` map
  positions back to what you built the paragraph from. For rich text, `RichLayout::text_pos` gives
  `decibell_richtext::plain::TextPos` (→ `Document::source_offset`), and `copy_text(from, to,
  document, source)` is the "copy as displayed" string.
- **An element never owns a selection.** `Layout::paint` takes `PaintOptions { selection:
  Some((from, to)), selection_background, selection_color, … }`; `RichText` takes
  `.selection(RichSelection::{None, All, Range(a, b)}, background, text_colour)`. Whoever owns the
  list keeps one anchor and one focus as (row, `RichPos`), finds them with `RichLayout::hit`, and
  hands every visible row its share (`All` for rows in between, `Range(layout.start(), pos)` for
  the last). `gallery/src/rich.rs` is that controller in 60 lines, with double and triple click.
- Runs with `selectable: false` (the "(edited)" marker) are skipped by the highlight.
- Chromium's default selection colours (the renderer has no `::selection` rule), measured in
  Electron 33 on the app's dark ground: background `#0538A4`, selected text `#FFFFFF`.

### Caches (all byte-bounded, two generations: what a frame uses survives, the rest is dropped)

| Cache | Key | Budget |
|---|---|---|
| Labels (`Engine::label_layout`, `Label`, `code_layout`) | text, style without colour, wrapping, scale → paragraph; + width → layout | 2 MB |
| Message bodies (`Engine::rich_layout`) | the caller's `key`, style, edited → paragraphs; + width, scale → layouts | 12 MB paragraphs + layouts, 3 MB `RichLayout`s |
| Formulas (`Engine::formula`) | TeX source, inline / display, text size, scale | 2 MB |
| Highlighted code (`Engine::highlighted`) | language, code | 1 MB |
| Emoji images (`engine.emoji`) | asset, device px | 8 MB of pixels; evicted textures are freed (`drop_image`) |

A layout a view holds (`Rc`) stays alive whatever the cache does. GPUI's own per-window line cache
is not used at all (`TextSystem::shape_line_uncached`, patch 0010): nothing is cached twice.
A colour is not part of a label's layout (it is given at paint), so hover and theme changes
re-shape nothing; a message body's colours are in its paragraphs, so a theme switch lays the
visible messages out again (28 µs each on average).

## Drawing text: three examples

**1. UI text** (through `decibell-ui`, which adds the theme):

```rust
use decibell_ui::{FontRole, Label};

Label::new("general").font(FontRole::Channel).size(theme.scaled.text_channel).weight(500.)
Label::new(topic).size(12.).color(theme.color_text_muted).truncate()      // one line, "…"
Label::new(bio).clamp(3)                                                  // line-clamp-3
Label::new("TEXT CHANNELS").font(FontRole::Section).size(11.).weight(600.).tracking(0.12)
```

Without the theme: `decibell_text::Label::new(text, TextStyle::new(face, 13., colour))` with
`.truncate()`, `.nowrap()`, `.clamp(n)`, `.wrapping(…)`, `.inherit_color()`.

**2. A message body** (the list measures, the row paints, with the same `Rc`):

```rust
let engine = Engine::global(cx);
let document = decibell_richtext::parse(&message.content, &ParseOptions::default()); // keep it in the model
let layout: Rc<RichLayout> = engine.rich_layout(
    message.id ^ message.edit_stamp,                       // names the content
    RichContent { document: &document, source: &message.content, edited: message.edited },
    &rich_style,                                           // a RichStyle built once per theme / text size
    column_width, window.scale_factor(),
    &mut |span, kind, width| my_block_height(span, kind, width),   // code blocks, display math
);
let row_height = layout.height();                          // exact, before anything is painted

RichText::new(("message", message.id), layout)
    .selection(selection_for_this_row, selection_background, Some(selection_text))
    .on_link(|span, _event, window, cx| open_link(span, window, cx))
    .slot(|slot, window, cx| code_block_or_math(slot, window, cx))   // your chrome, in the slot
```

**3. Your own paragraph** (anything inline: text runs, boxes, chips), measured without painting:

```rust
let mut builder = ParagraphBuilder::new(&engine, &base_style, Wrapping::PRE_WRAP, scale);
builder.text("replying to ", &muted, 0).text(&name, &bold, 1).inline_box(16., 16., 3., 2);
let paragraph = Rc::new(builder.build());                  // shaped; keep it
let layout = Rc::new(Layout::new(paragraph.clone(), width, &engine));
let (w, h) = (layout.width(), layout.height());
TextBlock::new(layout.clone())                             // or layout.paint(origin, &PaintOptions::default(), window, cx)
for (tag, [x, y, w, h]) in layout.boxes() { /* paint what box `tag` holds */ }
```

Code blocks: `engine.highlighted(language, code, cx, |cx| notify)` (the spans, within the frame
when quick, else from a background thread) → `engine.code_layout(code, spans, &code_style,
Wrapping::PRE, f32::INFINITY, scale)` → `TextBlock`. Display math: `engine.formula(tex, true,
text_px, scale)` → `Formula::Math(math)` → `MathBlock::new(math, colours)`; `math.width` against
the container's width says whether it overflows.

## What is reproduced from Chromium

Each rule is in the code with a comment where it applies, and `tools/README.md` lists them with
how they were found. In short: layout runs in device pixels on Blink's 1/64 px grid (so a
paragraph and a layout are made for one scale factor); line boxes are the paragraph's strut plus
every inline box, from hhea metrics rounded Blink's way with the half-leading floored; break
opportunities are UAX #14 over the whole paragraph (ICU4X, the tables GPUI already links) corrected
by Blink's own ASCII and `break-all` tables; trailing spaces hang; a chip's cloned padding
overflows as in Blink; `letter-spacing` is added per cluster and turns common ligatures off; a
line that ends inside a shaped run is measured as Blink's re-shaped line.

Pixel snapping: each line's baseline is rounded to a whole device pixel when painted and its chips
and decorations move with it; glyphs sit on a quarter-pixel grid horizontally (GPUI's subpixel
variants, Skia's grid too). `Layout::height()` is Chromium's unrounded height: 21.015625 px per
body line in Nocturne (14.5 px × 1.45, truncated to 1/64), so a row is a whole number of device
pixels exactly where the renderer's is; the list should place rows on whole device pixels and keep
the fraction in its running offset, as Chromium's layout does.

Bidi: within one shaped run GPUI's shaper orders glyphs visually, and the engine keeps that order
(Arabic and Hebrew read correctly, hit-testing lands on the right cluster). The paragraph
direction is always left to right, runs are placed in text order (a line that mixes directions
across several runs is not reordered as UAX #9 would), a line that breaks inside a
right-to-left run takes its first part first, and a selection across a direction boundary is one
span, not two.

## Fidelity against Chromium

`cargo test -p decibell-text --test fidelity` (vectors: `tests/data/fidelity.json.gz`, 607 KB,
made by `tools/`; Electron 33.4.11 = Chromium 130, scale 1 and 2). The corpus: every paragraph of
the 10k-message fixture at 560 px in Nocturne (Mona Sans 14.5 / 1.45), every fifth also at 320 and
900 px, at 13 and 16 px, and in Graphite (Inter 13 / 1.4); and 47 hand-written paragraphs × 4
styles × 3 widths as message bodies, plus `pre-wrap` + `break-word` and `white-space: normal`.

| | scale 1 | scale 2 |
|---|---|---|
| Fixture paragraphs with identical line breaks | **21,389 / 21,391 (99.991 %)** | 21,390 / 21,391 (99.995 %) |
| Hand-written cases with identical line breaks | 930 / 936 | 930 / 936 |
| Paragraph height, where the breaks agree | identical (max 0.0001 px) | identical |
| Top of every line, where the breaks agree | identical (max 0.0001 px) | identical |
| Line width: p99 / max difference | 0.0157 px (one LayoutUnit) / 0.14 px | 0.0079 / 0.14 px |

Every difference is listed in `tests/fidelity.rs` with its reason:

- 3 fixture paragraphs: a line that ends within 1/64 px of the box's edge, where Blink's float
  arithmetic and ours round differently (one of the three flips between scale 1 and 2 in
  Chromium itself).
- 6 hand-written cases, all the soft-hyphen paragraph outside message bodies: when the hyphen
  does not fit, Blink breaks the word where the line is full instead of going back to the
  previous opportunity. Message bodies (`break-all`) agree.
- Not compared: 168 cases with right-to-left text (the page's line detection needs x to grow) or
  scripts the bundled fonts lack (drawn by whatever font the machine has).

### Pixels and antialiasing

`tools/pixels.py` over the 12 cases of `tools/pixel-cases.json`: Chromium's capture of each against
the engine's through GPUI's headless renderer, both #ECE6DA on #17141C. It writes a sheet per case
(Chromium, the engine, the difference × 4); the ones from 2026-11-26 are in
`gpui-client/target-text/pixels/` (a build directory: regenerate with `tools/README.md`).

- **Chromium draws this text with LCD (subpixel) antialiasing** at scale 1 and at 2: 95 % and 85 %
  of the edge pixels of its captures carry colour fringes. GPUI uses LCD only when the window is
  opaque and the GPU has dual-source blending; the client's window is transparent (its
  client-side shadow), and the headless renderer never does, so **our text is greyscale**. The
  pictures therefore cannot match pixel for pixel; what can be compared is where the ink is and
  how much of it there is.
- Where: line tops identical, widths to 1/64 px (above); the sheets show the same glyph shapes at
  the same places, the synthetic italic at the same slant (0.25).
- How much (ink = the sum of every pixel's distance from the background, in full-ink pixels;
  engine ÷ Chromium over the 12 cases), and the mean per-pixel difference in luma (0–255):

  | GPUI setting (environment, read once per renderer) | ink, scale 1 | ink, scale 2 | mean difference, 1 / 2 |
  |---|---|---|---|
  | default (`ZED_FONTS_GAMMA` 1.8, greyscale enhanced contrast 1.0) | 1.089 | 1.054 | 4.43 / 3.54 |
  | `ZED_FONTS_GAMMA=1.0` | 0.921 | 0.964 | 4.19 / 3.44 |
  | **`ZED_FONTS_GAMMA=1.4`** | **1.014** | **1.013** | **4.10 / 3.40** |
  | `ZED_FONTS_GAMMA=2.2` | 1.150 | 1.085 | 4.86 / 3.71 |
  | `ZED_FONTS_GRAYSCALE_ENHANCED_CONTRAST=0` (or 0.5) | 1.087 | 1.052 | 4.42 / 3.54 |

  With GPUI's defaults our light-on-dark text is 9 % heavier than Chromium's at scale 1 and 5 % at
  scale 2. A gamma of 1.4 brings it within 1.5 % at both scales and is also the closest pixel for
  pixel; the contrast knob does nothing measurable for light text on a dark ground. There is no
  API for these: the client would set `ZED_FONTS_GAMMA=1.4` in its environment before the
  renderer starts, or carry a small patch. **Not applied: the owner should look at the two sets
  of sheets first** (`side-default-*` and `side-gamma1.4-*`).
- Advances are fractional at every scale in Chromium, as here (the widths show it). Whether
  Chromium also *paints* each glyph at its fractional x at scale 1, as GPUI does on a
  quarter-pixel grid, could not be told apart from its LCD filtering in these captures.

## Performance (i7-12700K, release, GPUI's Linux text system, bundled fonts)

`DECIBELL_FIXTURE_TEXTS=texts.json cargo bench -p decibell-text` (layout; a counting allocator),
`cargo test --release -p decibell-text --test paint -- --nocapture` (painting).

| | |
|---|---|
| Layout of a fixture message at 560 px, cold (parse excluded: shape + wrap) | median **16.8 µs**, p90 52, p99 213, max 442; mean 28 µs |
| Allocations per cold layout | median 190 (p99 1,922): almost all inside cosmic-text's shaper; the engine itself makes about a dozen (the paragraph's and the layout's arrays) |
| Retained per cached message | 3.0 KB (paragraph: text, 12 B per glyph, 16 B per caret unit; layout: lines and fragments) |
| Cold "open channel", 150 messages | 6.2 ms in one go (first-use costs included), 4.2 ms at the mean: **more than one 280 Hz frame (3.57 ms)**, so the list lays out the rows it shows first (25 rows ≈ 0.7 ms) and the rest in slices; a 16 ms budget buys about 570 messages |
| The same messages at another width (a resize) | no shaping at all: median **0.8 µs** per message (p99 8.6) while the paragraphs are cached |
| A cache hit (`rich_layout` for a row that has one) | 120 ns, 0 allocations |
| Painting 48 message rows (≈ 3,700 glyphs, selection on) from cached layouts | **1.0 ms per frame, 0 allocations** (asserted). 0.27 µs per glyph, nearly all of it inside GPUI's `paint_glyph` (two hash lookups under locks and a bounds-tree insert per glyph: what GPUI's own text pays too) |
| Emoji: look up + inflate the SVG | 4.8 µs |
| Emoji: rasterise (resvg) at 24 / 48 / 56 / 112 device px | median 50 / 68 / 74 / 109 µs (p90 about 2×) |
| Emoji cache for a busy screen | 24 px: 2.3 KB each; 30 distinct emoji on screen at scale 1: 69 KB (276 KB at scale 2); budget 8 MB |

Emoji never pop in: `rich_layout` rasterises a message's emoji while it builds the layout (50 µs
each at 24 px), which the list does before the row can be shown; painting only looks them up. An
evicted emoji is rasterised again inside the paint that needs it.

Startup and memory before / after the font patch (`patches/README.md`, 0011): first frame 277 →
**191 ms**, private dirty memory 30.9 → 30.2 MB, on the token gallery.

## Emoji

2,879 Twemoji SVGs (the ones the renderer's lookup can reach; 841 files that keep U+FE0F inside
a sequence are unreachable in the renderer too and left out) are packed by `build.rs` into one
blob, each SVG deflated on its own, with a sorted index: **2.60 MB in the binary** (8.1 MB of
SVG), file-backed pages that are only read where an emoji is used. A checkout without
`assets/twemoji` (run `node tools/fetch-assets.mjs`) builds with an empty pack and a warning, and
every emoji falls back to the system emoji font, which is also what an emoji without a Twemoji
drawing does (as in the renderer).

## Fonts and fallback

- The app starts with only the bundled faces in GPUI's font database (patch 0011). A character
  none of them has is looked up with fontconfig (`dlopen`ed at that moment, never linked: the
  same question Chromium asks), and that one font file is mapped and added; the text is then
  shaped again in the same layout call, so nothing is painted with a missing glyph first. Cost,
  measured: the first lookup of a process 21 ms (loading fontconfig and its caches, then the
  font), a later new font 1–4 ms, a character whose font is loaded nothing.
  `decibell_text::prewarm_system_fonts()` moves the first cost to any thread you like (after the
  first frame).
- A fallback font never changes a line box: lines are sized from the run's own face.
- KaTeX's 20 faces (0.5 MB in the binary) are handed to the text system at the first formula.
- Windows: DirectWrite has the system fonts and its own fallback; none of this runs there.

## Deferred, with pointers

- **Inputs** (composer, fields, IME): not started. `Layout` has what a field needs
  (`caret`, `hit`, `rects_for_range`, `word_at`, positions with affinity); what is missing is an
  editable buffer and `EntityInputHandler` glue (`docs/gpui-notes.md` §7).
- **Code block and display math chrome** (header, copy button, 14-line collapse, horizontal
  scroll): the chat view's. Building blocks: `Engine::highlighted`, `code_layout`, `TextBlock`,
  `MathBlock`, `RichText::slot`. `gallery/src/rich.rs` has a minimal version of both.
- **Selection controller for the message list**, clipboard, autoscroll: the list's;
  `gallery/src/rich.rs` shows the whole contract on three columns.
- **Inline formulas do not wrap**: a formula is one box. Chromium may break between KaTeX's
  bases (`MathLayout::bases`); `rich.rs` `rich_paragraph` would emit one box per base.
- **Emoji click target / popover, search highlight (`<mark>`)**: `RichLayout::hit` gives the
  span under a point; a highlight is a second kind of range to paint (`paint.rs`).
- **Soft hyphens outside message bodies** follow CSS, not Blink's quirk (above).
- **`text-decoration` under a selection** takes the selection colour only when the whole run is
  selected.
- **Synthetic bold inside formulas** goes through the same font twin as bold text (FreeType's
  1/24 em emboldening, as Skia does on Linux); `\pmb`'s shadow copies are drawn as glyphs.
- **A glyph-run primitive in GPUI** would cut painting cost several times (one scene insert per
  run instead of per glyph); not attempted.

## Windows

Everything compiles for `cfg(windows)` by construction (the Linux-only parts are `cfg`-gated:
`fallback.rs`, the word joiners in `engine.rs`), but nothing here was run there. Unverified:

- Line breaks and widths: the vectors were checked against GPUI's Linux shaper only. DirectWrite
  shapes whole runs (no joiners needed) and may kern or round differently.
- Synthetic italics and bold: `IDWriteFontSet::GetMatchingFonts` is expected to return
  DirectWrite's simulated oblique (20°, not Skia's 14°) and bold for a family without those
  faces; patch 0010's twin fonts are Linux-only. If italics come out upright or too slanted,
  `gpui_windows/src/direct_write.rs` `glyph_analysis` takes a transform.
- `Face::line_metrics` uses GPUI's `FontMetrics`, which on Windows come from DirectWrite's design
  metrics; they should be the same hhea numbers.
- The fidelity and paint tests and the examples are Linux-only (`cfg`), since they build GPUI's
  Linux text system directly.

## Layout of the crate

```
src/engine.rs      Engine: faces, shaping (word joiners, fallback), counters
src/font.rs        FontSpec, Face, Blink's metric rounding
src/style.rs       TextStyle, LineHeight, Chip, Wrapping
src/paragraph.rs   ParagraphBuilder: shaping, units, break opportunities
src/blink_breaks.rs  generated: Blink's ASCII break tables
src/layout.rs      line breaking, line boxes, fragments, chips and decorations
src/paint.rs       Layout::paint, PaintOptions
src/hit.rs         positions, hit-testing, ranges, words
src/cache.rs       the two-generation caches and their keys
src/fallback.rs    Linux: system fonts on demand (fontconfig by dlopen)
src/emoji.rs       the Twemoji pack, rasteriser, image cache
src/math.rs        formulas: decibell-tex's display list → paintable
src/code.rs        highlighted code: background highlighting, code paragraphs
src/label.rs       Label, TextBlock, MathBlock
src/rich.rs        RichStyle, RichLayout, RichText
gallery/           the text-roles and rich-text screens (its own crate: it uses decibell-ui)
tools/             the Chromium harness (tools/README.md)
```
