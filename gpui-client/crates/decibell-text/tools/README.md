# decibell-text tools: the Chromium comparison

The engine's bar is "the same lines, the same heights, the same widths as Chromium". These tools
ask Chromium itself (the renderer's own, `electron-client/node_modules/electron`, Electron 33)
and turn its answers into the vectors `cargo test -p decibell-text` checks without Electron.

Nothing here touches the Decibell app, its profile or the network: `chromium/run.sh` starts
Electron with a throwaway `--user-data-dir` on a local page.

## The pieces

| File | What |
|---|---|
| `fixture-texts.py` | Prints the 10k-message fixture's sources (same generator and seed as `tools/dev/fixture.py`) as a JSON array. |
| `../examples/corpus.rs` | Parses every source with `decibell-richtext` and writes the corpus: per paragraph its *segments* (`[flags, text]` runs and `["box", w, h, below_em]` emoji boxes) at several widths, sizes and themes, plus a hand-written set (long words, URLs, tabs, soft hyphens, chips at line ends, emoji at line starts, CJK, RTL, …) in three wrapping modes. |
| `chromium/page.html` | Builds each case as the renderer's markup with the renderer's CSS (`whitespace-pre-wrap break-all [overflow-wrap:anywhere]`, `<strong>` / `<em>` / `<code class=chip>` …) and the bundled static fonts, and records per line: where it starts, how wide it is, where its first character's box starts; and the paragraph's height. |
| `chromium/run.sh`, `main.js`, `preload.js` | Run a page in Electron at a device scale factor; `--shots <dir>` also saves a PNG per case (the window is shown for that). |
| `merge-vectors.py` | Packs the corpus and the results into `../tests/data/fidelity.json.gz`. |
| `../examples/fidelity.rs` | The development view: lays the corpus out with the engine and prints every difference (`--lines`, `--widths`, `--heights`, `--edges`, `--filter <id>`, `--dump`). |
| `../tests/fidelity.rs` | The committed check. Differences that are understood are listed there with their reason; anything else fails. |
| `chromium/metrics.html` | Blink's rounded ascent / descent per font and size (the rule in `Face::line_metrics`). |
| `chromium/pairs.html`, `gen-blink-breaks.py` | Blink's ASCII line-break table for `word-break: normal` and `break-all` → `../src/blink_breaks.rs`. |
| `pixel-cases.json`, `../examples/render.rs`, `pixels.py` | A dozen cases rendered by both sides and compared pixel by pixel. |

## Regenerating the vectors

```sh
cd gpui-client
T=$(mktemp -d)
python3 crates/decibell-text/tools/fixture-texts.py > $T/texts.json
cargo run --release -p decibell-text --example corpus -- --fixture $T/texts.json --out $T/corpus.json
crates/decibell-text/tools/chromium/run.sh $T/corpus.json $T/chromium-1.json --dsf 1     # ~5 s each
crates/decibell-text/tools/chromium/run.sh $T/corpus.json $T/chromium-2.json --dsf 2
cargo run --release -p decibell-text --example fidelity -- $T/corpus.json $T/chromium-1.json --lines
python3 crates/decibell-text/tools/merge-vectors.py $T/corpus.json $T/chromium-1.json $T/chromium-2.json \
    > crates/decibell-text/tests/data/fidelity.json.gz
cargo test --release -p decibell-text --test fidelity -- --nocapture
```

The other tables (run when Electron's Chromium changes):

```sh
echo '{}' > $T/empty.json
crates/decibell-text/tools/chromium/run.sh $T/empty.json $T/pairs.json --page pairs.html
python3 crates/decibell-text/tools/gen-blink-breaks.py $T/pairs.json > crates/decibell-text/src/blink_breaks.rs
echo '{"families":["Decibell Sans","Decibell Inter","Decibell Mono"],"sizes":[11.05,12.325,13,14.5,16]}' > $T/m.json
crates/decibell-text/tools/chromium/run.sh $T/m.json $T/metrics.json --dsf 2 --page metrics.html
```

## Input vectors

`chromium/input-keys.js` (run by `chromium/input-keys.sh <out.json>`, ~30 s, a hidden window, a
throwaway profile, no clipboard) presses real keys in a `<textarea>` and an `<input>` with
`webContents.sendInputEvent` and records what Blink's editing did: the caret stops of Left / Right
and Ctrl+Left / Right, the values after Backspace / Delete and their Ctrl variants, the undo and
redo steps of typed sequences, and a handful of single facts (selection collapse, Up / Down at the
ends, Home / End, the goal column, pasted line breaks, `maxlength`).

```sh
crates/decibell-text/tools/chromium/input-keys.sh crates/decibell-text/tests/data/chromium-input.json
cargo test -p decibell-text --test input_vectors && cargo test -p decibell-text --lib input
```

Offsets in the file are UTF-16 code units (`selectionStart`); the tests convert. To add a case, add
the string or the step list in `input-keys.js` and rerun: a new string is checked automatically,
a new kind of step needs a line in `tests/input_vectors.rs`.

## Pixels

```sh
crates/decibell-text/tools/chromium/run.sh crates/decibell-text/tools/pixel-cases.json $T/c.json --dsf 1 --shots $T/chromium
cargo run --release -p decibell-text --example render -- crates/decibell-text/tools/pixel-cases.json $T/ours --scale 1
python3 crates/decibell-text/tools/pixels.py $T/chromium $T/ours 1 $T/side-by-side
```

`pixels.py` aligns each pair, writes `<case>@<scale>x.png` (Chromium on top, the engine below,
the difference ×4 at the bottom) and prints the coverage ratio (how much ink each side lays
down) and the mean and maximum difference. `ZED_FONTS_GAMMA` /
`ZED_FONTS_GRAYSCALE_ENHANCED_CONTRAST` in the environment of the `render` example change GPUI's
glyph blending; the README's antialiasing section has what they do.

## What the corpus taught (each is a rule in the engine, with a comment where it lives)

- Blink lays text out in device pixels on a 1/64 px grid: a line height is truncated to it (a
  unitless `line-height` multiplies the font size *after* that was truncated), a fragment's
  width is rounded up to it, a font size is truncated to it before shaping.
- Ascent and descent are the hhea metrics rounded to whole device pixels; above scale 1 a
  descent that was rounded down takes a pixel from the ascent. The half-leading above the
  text is rounded down to a whole pixel. Fallback fonts change no line box.
- A line may pass the box's edge by one LayoutUnit; a line that ends inside a shaped run is
  measured again without the last glyph's kerning.
- `letter-spacing` turns `liga` and `clig` off, and is not added after zero-width characters.
- `box-decoration-break: clone`: the line breaker counts a chip's opening padding on every
  line but its closing padding only where the chip ends, so a chip cut by a line end pokes
  out of the box by 6 px.
- ASCII pairs break by Blink's own table, not UAX #14; `break-all` adds class pairs UAX #14's
  break-all does not have (before `-`, before `(`); `line-break: auto` is "normal"
  strictness (a line may start with small kana).
- Tabs: stops every `tab-size` × the space's advance without letter-spacing, from the line's
  start; a stop closer than half a space is skipped.
- GPUI's Linux shaper cuts a line into words at every break opportunity and never kerns
  across a cut (`/` + letter, `://`); a WORD JOINER at each cut restores Chromium's widths.
