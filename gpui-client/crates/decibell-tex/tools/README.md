# decibell-tex tools: corpus, markup vectors, Chromium reference harness

Ground truth for the KaTeX port. Everything here needs Node (and Electron for the harness) to
regenerate. `cargo test` only reads the committed `tests/data/*.jsonl.gz`.

| Path | What it is |
|---|---|
| `corpus/build_corpus.mjs` | Builds `corpus/corpus.jsonl` (one `{"id","src","tex","display"}` per line, deduplicated on tex + display). |
| `corpus/extract_spec.mjs` | Pulls TeX inputs out of KaTeX's `test/katex-spec.ts` with a small JS lexer (no code is executed). |
| `corpus/katex_internals.mjs` | Bundles `electron-client/node_modules/katex/src` in memory with esbuild to read `_functions`, `_environments`, `symbols`, `macros`; the app's render options. |
| `corpus/handwritten.txt` | Hand-written formulas: everyday maths, every construct family, errors, adversarial. `## section`, `# comment`, prefix `D:` display only, `B:` both modes. |
| `corpus/upstream/` | Cache of `katex-spec.ts` and `ss_data.yaml` fetched from GitHub at tag v0.18.4 (gitignored). |
| `gen_markup.mjs` | Writes `tests/data/markup.jsonl.gz`: KaTeX's HTML for every corpus formula. |
| `harness/` | Electron reference harness: `run.sh` → `main.cjs` (hidden window) → `page.html` + `measure.js`. Writes `tests/data/layout.jsonl.gz`. |

## Regenerating

```sh
cd gpui-client/crates/decibell-tex
node tools/corpus/build_corpus.mjs            # --offline: use corpus/upstream/ only, never fetch
node tools/gen_markup.mjs                     # [corpus.jsonl] [out.jsonl.gz]
tools/harness/run.sh tools/corpus/corpus.jsonl tests/data/layout.jsonl.gz        # DSF 2 (default)
tools/harness/run.sh tools/corpus/corpus.jsonl /tmp/x.jsonl --dsf 1 --filter '^hw-roots'   # debugging
```

KaTeX, Electron and esbuild all come from `electron-client/node_modules` (KaTeX 0.18.4,
castlabs Electron 33.4.11 = Chromium 130). The full corpus takes under a second in Node and
about 5 s in the harness. `run.sh` options: `--dsf N` (device scale factor, default 2),
`--filter <regex on ids>`, `--batch N`; env `HARNESS_TMP` = where the throwaway profile goes
(removed on exit), `KATEX_DIST`, `ELECTRON`. The window is never shown, the session is an
in-memory partition, and every non-`file:`/`data:` request is cancelled.

## Ids

`<src>-<hint>[-D]`: `spec` (katex-spec.ts), `ss` (screenshotter data; macros / styles / pre /
post options dropped), `fn` (every `_functions` entry with generated arguments), `env` (every
environment, display and inline), `sym` (every math / text symbol; plain letters batched 8 per
formula), `macro` (every built-in macro, also inside `\text{}`), `hw` (handwritten.txt
`section-NNN`), `adv` (generated adversarial input). `-D` = display mode.

## markup.jsonl.gz

`{"id","tex","display","status","html","ms"}` — `katex.renderToString(tex, {displayMode,
throwOnError:false, errorColor:"#cc0000", trust:false, maxSize:16, maxExpand:256})`, the app's
options (`MathTex.tsx`) except the colour (the app passes `var(--color-error)`).

- `ok`: the output with `<span class="katex-mathml">…</span>` removed (screen-reader only;
  the script checks it holds no nested span). Starts `<span class="katex">` (inline) or
  `<span class="katex-display"><span class="katex">` (display).
- `error`: the whole `<span class="katex-error" title="ParseError: …" style="color:#cc0000">tex</span>`.
- `throw`: renderToString threw a non-ParseError (e.g. `Error: Font metrics not found for font:
  Typewriter-Bold.` for `\texttt{\textbf{x}}`); `html` is `name: message`. The app then shows
  the raw TeX in a `<code>`.
- `ms`: one render's wall time after a warm-up pass.

## layout.jsonl.gz

Line 1: `{"meta":{katex, dpr, ua, hostFontPx:16, inlineWidth:20000, displayWidth:1000, count,
calibration:[…]}}`. Then one record per formula:

- Host markup, as in the app: inline formulas in
  `<div style="font-size:16px;width:20000px;line-height:normal"><span class="rich-math-inline">…`,
  display formulas in `<div style="font-size:16px;width:1000px"><div class="rich-math-display">…`
  with the app's overrides (`.katex-display` margin 0 and left-aligned). `.katex` is 1.21em =
  19.36px. Every host starts on a whole-pixel y.
- **Origin**: x = the host's left content edge (where the formula starts), y = the baseline of
  the first `.katex-base` (a 0×0 inline-block probe inserted as its first child). All
  coordinates are CSS px relative to it, y down. Numbers keep 7 decimals, so LayoutUnit values
  are exact.
- `status` as in markup; `hh` = FNV-1a 32 over the UTF-16 code units of the measured markup
  (MathML removed) — equal to the hash of `markup.jsonl.gz`'s `html` for the same id.
- `error` records: `error` (title text), `box` (the error span relative to the host's top-left),
  `errorFont` (host font: family, px, colour). `throw` records: `error` only.
- `box`: `.katex-html` rect; `bases`: each `.katex-base` rect; `tag`: `.katex-tag` rect or null;
  `newlines`: count of `.katex-newline`; `host`: host width/height, origin within the host,
  `outer` = the `.rich-math-*` wrapper's rect (inline: the inline box of `.katex`'s line).
- `glyphs`: per code point of every text node in `.katex-html` (except the U+200B in
  `.vlist-s`): `t` char, `n` text-node index (glyphs with the same `n` are one shaped run),
  `x` left and `w` width of its Range rect (the advance box), `y` baseline (from a 0×0 probe
  inserted before the text node), `top`/`bot` the Range rect's vertical extent (Chromium's
  rounded font ascent/descent, not ink), and the parent's computed `f` first font-family, `s`
  font-size px, `i` italic, `b` weight, `c` colour, `cls` class list.
- `rects`: every element with a painted border side or a background: border-box rect, `border`
  per side `{w, s(tyle), c}` (computed, i.e. after Chromium's device-pixel snapping),
  `bg`, `cls`.
- `svgs`: every `<svg>`: rect, `clip` = intersection of its `overflow:hidden` ancestors inside
  `.katex-html` (or null), `viewBox`, `par` (preserveAspectRatio), `wa`/`ha` (width / height
  attributes), `c` colour, `pcls` parent classes, `paths` `[{h: FNV-1a of d, n: d length}]`,
  `lines` (`\cancel` strokes) when present.
- `probeShift`: max movement of any glyph / the `.katex-html` box caused by the probes (0 for
  the whole committed set).

## What Chromium does (Electron 33 / Chromium 130 on Linux; evidence in the vectors)

- **Advances are fractional at DSF 1 and 2** (subpixel positioning is on); no kerning or
  ligatures (the KaTeX TTFs have no GSUB/GPOS/kern), and the woff2 faces Chromium loads have
  the same cmap and hmtx as the TTFs.
- **Font size for shaping is truncated to 1/64 device px** (FreeType 26.6 char size): advance =
  hmtx / 1000 × floor(px × 64 × DSF) / (64 × DSF). At 19.36 px that is 19.359375. Within a text
  run, glyph Range lefts = floor(run left + Σ advances − ε) on the LayoutUnit grid: 8251 of 8525
  in-run glyph lefts match this exactly at DSF 2, the rest within 3 LayoutUnits; with the
  untruncated size the error grows along a run (up to −18 LU).
- **LayoutUnit = 1/(64 × DSF) CSS px**: at DSF 2 every rect / glyph coordinate in the set is a
  multiple of 1/128 px; at DSF 1 of 1/64. Box widths accumulate rounding along a line, so DSF 1
  and DSF 2 disagree by up to ~0.3 px at the end of very long formulas (≤ 1/32 px for typical
  ones). DSF 2 is the committed setting (finer grid).
- **Font ascent/descent are rounded to device pixels** (Skia metrics, then Blink rounds each;
  if the descent was rounded down it borrows 1 px from the ascent). E.g. KaTeX_Math at 19.36 px,
  DSF 2: 0.717 em → 27.76 dev px and 0.218 em → 8.44 dev px give glyph boxes −13.5 … +4.5 px;
  KaTeX_Main −17.5 … +5.5. These set the inline / line box heights (`.katex-base` rects,
  `host.h`), not glyph positions.
- **Border widths snap to device pixels** (floor, minimum one device pixel): KaTeX's 0.04em
  rules (0.774 px) paint 0.5 px thick at DSF 2 and 1 px at DSF 1, and `.frac-line` etc. also have
  `min-height: 1px`. Vertical separators and `\hline`s therefore move later content by DSF.
- **Synthetic bold / oblique** (a weight or style the family lacks: KaTeX_AMS, Typewriter,
  Size1, Script at 700 or italic) do not change advances (`meta.calibration`). KaTeX itself
  never selects them in the corpus: where it would, it throws for missing metrics instead.
- **Fallback glyphs**: 125 glyphs in the corpus are code points absent from the face their
  span selects (Cyrillic, CJK, Arabic digits, U+22B6/7, U+FE0F, …); Chromium draws them with a
  system font, so their advances here are not KaTeX's.
