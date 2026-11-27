# decibell-text

The text engine of the GPUI client: every label, message body and input is laid out and
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

## Inputs (`decibell_text::input`)

GPUI ships no text field, only the IME protocol. This module is the client's: an editing engine
for short texts and the two fields built on it. The Electron client defines the behaviour
(`RichInput.tsx`, every `<input>`); Chromium's own editing was recorded with real key events and
is checked as vectors (below).

| Type | What it is |
|---|---|
| `Buffer` | The text (a `String`; a message is a few KB at most), the selection (`Selection { anchor, head }` in bytes), the `Rules`, the atoms, the IME's marked range and the undo history. No GPUI, no layout: plain data, unit-tested. |
| `Rules` | `multiline`, `max_utf16` (HTML `maxlength`), `max_utf8` (the renderer's `clampLine`: settings fields under the server's byte caps), `accept` (a character filter: `numeric`), `emoji_atoms` (the composer), `masked` (a password), `read_only`. |
| `InputState` | The field as a GPUI entity **and view**: buffer + focus handle + layout + caret blink + mouse + clipboard + `EntityInputHandler` (IME). The view that owns a field makes one and keeps it. |
| `TextInput`, `TextArea` | What goes in the element tree: `TextInput::new(&state, style)` (one line, scrolls sideways), `TextArea::new(&state, style)` (wraps, as high as its text). In the app both are reached through `decibell_ui::TextField::new(id).editor(&state)`, which is the box around them. |
| `InputStyle` | Text style, placeholder / selection / caret colours, wrapping, atom size. No theme (`decibell_ui::TextField::input_style(font, size, leading, cx)` fills it from the theme). |
| `InputEvent` | `Change`, `Submit`, `Escape`, `ArrowUpEmpty`, `Focus`, `Blur`: `cx.subscribe(&state, …)`. Delivered after the edit, so a handler may read and change the input. `Blur` / `Focus` also come when the window is deactivated / activated with the field focused (GPUI's focus events follow the window): a composition in progress is then kept as text. |
| `Enter` | What Enter does: `Submit` (a single line), `Newline`, `Chat` (Enter submits, Shift+Enter and an open ``` fence break the line), `Custom(fn(text, shift) -> submit?)`. |

### The handle

`InputState` is the imperative handle (`RichInputHandle`): `value()`, `set_value(text, cx)`,
`clear(cx)`, `insert_text(text, cx)` (over the selection, one undo step: the emoji picker, a
mention), `selection()`, `select(…)`, `select_all(cx)`, `focus(window, cx)` (caret to the end),
`is_empty()` / `is_blank()`, `is_composing()`, `caret_bounds()` (window coordinates: where to
anchor a popup), `focus_handle(cx)`. Hooks for what needs an answer on the spot:
`on_escape(|window, cx| handled?)`, `on_paste_non_text(|item, window, cx| taken?)` (images and
files on the clipboard: the composer's attachments), `pass_keys(|keystroke| leave_alone?)` (a
list's filter box lets ↑ ↓ Enter through), `on_reveal_caret(|rect, cx| …)` (a scroll container
keeps the caret in view), `set_colorizer(Some(code_colorizer(lang, palette)), cx)` (styled byte
ranges of the whole text, asked once per edit: the code panel's live highlighting; only lines whose
ranges changed are shaped again). A multi-line field with `InputStyle::wrapping = Wrapping::PRE`
doesn't wrap and is as wide as its longest line (its container scrolls it sideways).
`type_to_focus(&key_down, window, cx) -> bool` is "type anywhere to
compose": call it from the chat view's key listener when no field has focus; a printable key
focuses the input and lands in it. `InputEvent::ArrowUpEmpty` is ArrowUp in a blank composer
(edit the last message).

```rust
// A settings field, capped as the community server caps it. In the view's constructor:
let name = cx.new(|cx| InputState::single_line(window, cx).placeholder("Server name").max_bytes(64).with_value(&server.name));
cx.subscribe(&name, |this, name, event: &InputEvent, cx| match event {
    InputEvent::Change => this.draft.name = name.read(cx).value().to_string(),
    InputEvent::Submit => this.save(cx),
    _ => {}
}).detach();
// In render:
Field::new("Server name").child(TextField::new("server-name").editor(&self.name).error(self.name_error.is_some()))
```

```rust
// The composer's input. In the composer view's constructor:
let input = cx.new(|cx| InputState::composer(window, cx).placeholder(format!("Message #{channel}")));
input.update(cx, |input, _| {
    input.on_escape(move |_, cx| drop_pending_reply(cx));                       // true swallows Escape
    input.on_paste_non_text(move |item, window, cx| attach_from_clipboard(item, window, cx));
});
cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| match event {
    InputEvent::Submit => { let text = input.read(cx).value().to_string(); input.update(cx, |i, cx| i.clear(cx)); this.send(text, window, cx) }
    InputEvent::ArrowUpEmpty => this.edit_last_message(window, cx),
    InputEvent::Change => this.draft_changed(input.read(cx).value(), cx),      // preview strip: composer::preview_visible
    _ => {}
}).detach();
// In render: the renderer's `.rich-input` (channel font and size, line-height 1.4), growing to 160 px:
let style = TextField::input_style(FontRole::Channel, theme.scaled.text_channel, 1.4, cx);
ScrollArea::new("composer-input").track_scroll(&self.scroll).max_h(px(160.)).child(TextArea::new(&self.input, style))
// + input.on_reveal_caret(|caret, _| scroll.scroll_into_view(caret.top(), caret.size.height, ScrollAlign::Nearest))
// The picker: self.input.update(cx, |input, cx| { input.insert_text("😀", cx); input.focus(window, cx) });
```

### What it does

- **Keys** (`keymap.rs`; Linux and Windows agree, macOS uses Cmd / Option): arrows, Ctrl+arrows by
  word, Home / End (the visual line), Ctrl+Home / End, Page Up / Down, Shift to extend, Ctrl+A,
  Backspace / Delete, Ctrl+Backspace / Delete, Ctrl+C / X / V, Ctrl+Insert, Shift+Insert,
  Shift+Delete, Ctrl+Z, Ctrl+Shift+Z, Ctrl+Y, Enter, Escape. The field matches keystrokes in its
  own key-down listener (no global key bindings, no `init`): any other key propagates, so the
  platform or the IME inserts its text and an application binding on it still works. Up / Down
  keep a goal column across wrapped lines and lines of different lengths; in a single line they
  go to the ends, as in Chromium's `<input>`.
- **Mouse**: click, Shift+click, drag (the field keeps scrolling towards a pointer that left it),
  double click (word) and triple click (line) with drags that extend by words / lines, and on
  Linux the primary selection (published on selection, pasted by the middle button).
- **Undo / redo** as Chromium groups them (`history.rs`): a typing burst is one step however long
  the pauses; a Backspace or Delete starts a step that the typing after it joins; a caret move, a
  paste, a cut, a programmatic insert and a blur end the step; an IME composition is one step.
  1,000 steps / 1 MB, stored as the changed text only.
- **Rules**: a single line takes pasted line breaks as spaces (Chromium); `max_length` cuts the
  insertion on a character boundary to fit the UTF-16 budget (Chromium's `maxlength`); `max_bytes`
  is `lib/utf8.ts` `clampLine` exactly (line breaks and tabs → one space, control characters
  removed, **the value's tail** cut on a character boundary); `accept` filters characters.
- **Atoms**: with `emoji_atoms`, every emoji `decibell_richtext::composer::emoji_atoms` finds is one
  caret step and one deletion, drawn as a Twemoji box (`.rich-emoji`: 1.375 em, −0.3 em, 1 px
  margins) through the engine's inline boxes, and copied / read (`value()`) as its characters. A
  known `:shortcode:` becomes its emoji when its closing colon is typed (and on `set_value`), in
  the same undo step. Only the edited lines are scanned.
- **Password**: drawn as bullets (the secret is in no paragraph, no layout and no cache key: a
  password's layout key is its character count and byte widths), `selected_text()` is `None` (copy
  and cut do nothing), Ctrl+arrows see one word, the IME gets no text (`text_for_range` → `None`).
- **IME**: `EntityInputHandler` with UTF-16 ↔ UTF-8 at the boundary: marked text (drawn underlined
  in place), `bounds_for_range` for the candidate window (re-reported when the caret moves, from
  paint: `invalidate_character_coordinates`), commit, cancel (an empty preedit), unmark on blur
  (the preedit stays as text). Enter and Escape are the IME's while it composes.
- **The caret** is a rectangle, 1 px (at least one device pixel), in the text colour
  (`caret-color: auto`). It blinks at Blink's rate (500 ms shown, 500 ms hidden) from one timer task
  that exists only while the field is focused **and** its window is active; typing and moving only
  move the next blink's time (no task restart). No caret and no frames with a range selected,
  blurred, or in an inactive window.
- **Layout** (`display.rs`): one `Paragraph` + `Layout` per hard line. An edit shapes the line it
  happened in; the others are matched by a hash of their text and keep their layouts; a new width
  wraps the existing paragraphs again and shapes nothing. `InputState` is its own view: a key
  re-renders it and its ancestors, never its siblings.

### Against Chromium

`tools/chromium/input-keys.js` drives a hidden Electron page (`<textarea>`, `<input>`) with
`webContents.sendInputEvent` (Blink's own key handling) and records caret stops, values and undo
steps: `tests/data/chromium-input.json` (Chromium 130, Linux), checked by
`cargo test -p decibell-text --test input_vectors` and `input::tests`:

| | Cases | Result |
|---|---|---|
| Ctrl+Left / Right stops, Ctrl+Backspace / Delete results | 16 strings (punctuation, URLs, numbers, Turkish, emoji, line breaks, leading / trailing spaces) | 15 identical; 1 differs (below) |
| Left / Right stops, Backspace and Delete results | 8 strings (ZWJ family, flags, keycap, skin tone, combining marks, Hangul, Thai) | identical |
| Undo / redo steps | 11 sequences (bursts with 1.5 s and 4 s pauses, Backspace in a burst, caret moves, Enter, Ctrl+A, word delete) | identical |
| Selection collapse, word moves from a selection, Up / Down in an `<input>` and at a textarea's first / last line, Home / End, the goal column, pasted line breaks, `maxlength` | 17 | identical |

What the vectors taught (each is in the code where it applies): Ctrl+arrows skip spaces and stop at
words, at **runs of punctuation** and at each line break; a full stop or colon between letters
breaks a word ("example.com" is three stops) but not between digits; Backspace takes one code
point off a combining sequence but a whole emoji sequence, while Delete takes the grapheme;
Chromium has no undo timer.

Known differences:

- **Words in Chinese, Japanese and Thai**: Chromium uses ICU's dictionaries (日本語 | の | テキスト);
  here a run of such letters is one word. The dictionaries are about 2 MB of data.
- **Windows' Ctrl+Right** lands before the next word (Chromium's Windows behaviour); written
  (`next_word(.., skip_space)`), not recorded from a Windows Chromium.
- **A triple click** selects the hard line without its line break (Chromium's selection runs to
  the start of the next paragraph).
- **Caret blink**: Blink's default 500 ms. Chromium on Linux takes GTK's `gtk-cursor-blink-time`
  when it is set and stops blinking after `gtk-cursor-blink-timeout`; not read here.
- **Selection colours** are Chromium's defaults as measured on the dark themes (`#0538A4` /
  white) in every theme; an unfocused field shows no selection.
- **Drag autoscroll** moves about a character (or a line) every 50 ms; Chromium's speed depends on
  how far outside the pointer is.
- **Bidi**: as the engine (above): one direction per paragraph; the caret moves in logical order.

### Cost (i7-12700K, release; `cargo test --release -p decibell-text --test input_cost -- --nocapture`, `decibell --selftest input-typing,input-caret-idle`)

| | |
|---|---|
| Frames per typed character, caret move, preedit update, new line | **1** (2 for the first key after a click: GPUI redraws once when input changes from pointer to keyboard) |
| Typing a 200-character message into a `TextArea`, per key | edit (key dispatch → buffer → re-shape the line → layout) **≈ 50–70 µs**, 84 allocations (58 at 20 characters, 116 at 190: cosmic-text's shaper, per shaped run); the frame that shows it **≈ 17–35 µs**, 13 allocations (the element tree of the test window) |
| The same key with 8 KB in 40 other lines above it | 62 allocations (no more than alone); 1 line shaped, 40 reused; edit ≈ 110 µs (hashing the lines to match them: about 5 µs per KB) |
| A width change | every line wrapped again, nothing shaped |
| Focused, window active, idle | 2 frames/s (the blink), nothing else: 10 frames in 5 s; 30 ms of CPU in those 5 s (0.6 % of a core, all threads, release build, measured on X11 where the test window can be activated) |
| Blurred, or focused in an inactive window | **0 frames in 5 s**, no timer |
| Memory per field | `InputState` 1,032 bytes + 2.5 KB of heap (entity, focus handle, three subscriptions) empty; 6.8 KB with a 200-character message typed (text, undo step, paragraph, layout) |

### Windows, IME, and what was not run

- **A live IME was not run** (no fcitx / ibus in this session): the self-test `input-ime` drives the
  same `EntityInputHandler` methods a platform calls (preedit updates without ranges, as Wayland
  sends them; UTF-16 ranges; candidate bounds; commit; cancel). To check by hand: fcitx5 or ibus
  with a Japanese or Chinese engine in `decibell --screen inputs`: the preedit underlined in place,
  the candidate window under the caret (also on a wrapped line of the text area and in a
  scrolled single line), Enter committing without sending.
- **Windows**: nothing here is platform code except `cfg`s (`paste_primary`, the word-right rule);
  GPUI's IMM32 path calls the same handler. Not run: IME (IMM32; no TSF in GPUI), the clipboard,
  DirectWrite's shaping under the caret geometry, Ctrl+Right's Windows stops.
- **Wayland refuses a clipboard write without the serial of a real key or button press**, so
  `input-clipboard` and `input-password` run their clipboard round trip only on X11
  (`WAYLAND_DISPLAY= decibell --selftest input-clipboard`); with a real key press (a user) GPUI's
  Wayland clipboard works as for any GPUI app.
- GPUI on Linux writes only text to the clipboard and reads images; a pasted image reaches
  `on_paste_non_text` (checked on X11).

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

- **The composer around the input** (the preview strip, the emoji / GIF pickers, attachments,
  slowmode, send pacing): the chat view's. "Inputs" below says what the input gives it.
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
src/input/         text input: buffer, undo, key map, display, InputState, TextInput / TextArea
gallery/           the text-roles, rich-text and inputs screens and the inputs' self-tests (its own
                   crate: it uses decibell-ui)
tools/             the Chromium harness (tools/README.md)
```
