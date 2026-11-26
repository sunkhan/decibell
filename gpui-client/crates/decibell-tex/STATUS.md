# decibell-tex status

TeX math layout for the native client: KaTeX 0.18.4 ported to Rust (parser, macro expander,
every function and environment, HTML builder), plus the CSS layout Chromium performs on KaTeX's
markup with `katex.css`. Output: a positioned display list (glyphs with their KaTeX face, rects,
SVG paths). No GPUI, no I/O, no dependencies.

## What is ported (KaTeX `src/` → `src/`)

| KaTeX | Rust | Notes |
|---|---|---|
| Lexer, MacroExpander, Namespace, macros, defineMacro | `lexer.rs`, `macro_expander.rs`, `namespace.rs`, `macros.rs`, `data/macros_data.rs` | the token regex written out by hand; 317 string macros generated, 20 function macros by name |
| Parser, parseTree, ParseError, Token, SourceLocation, Settings | `parser.rs`, `build_tree.rs`, `parse_error.rs`, `token.rs`, `settings.rs` | Decibell's settings only (see below) |
| types/nodes, parseNode, defineFunction, defineEnvironment, functions, environments | `parse_node.rs`, `define_function.rs`, `functions/*.rs` (one per file, plus `assemble_sup_sub.rs`), `environments/{array,cd}.rs` | every `defineFunction` / `defineEnvironment`; name lookup is the sorted static `functions/table.rs` |
| buildHTML, buildCommon, domTree, tree, Options, Style, units, spacingData, fontMetrics(+Data), symbols, delimiter, stretchy, svgGeometry, wide-character, unicode* | same names | data tables generated (`tools/gen_tables.mjs`, `tools/gen_font_tables.py`) |
| `katex.css` + Chromium | `layout/` (`css.rs` cascade, `engine.rs` LayoutNG model, `text.rs` font matching/metrics, `color.rs`) | see "Layout model" |

Not ported (invisible or unreachable with Decibell's options): the MathML builders
(`buildMathML`, `mathMLTree`, every `mathmlBuilder`), `stretchyMathML`, `throwOnError: true`,
`trust` other than false, `strict: "error"`, user `macros`, `output: "mathml"`. The `\html*`,
`\includegraphics` builders are unreachable without trust and report an internal error.

## Verification

`cargo test -p decibell-tex` (no Node/Electron needed; vectors committed in `tests/data/`):

- **Markup** (`tests/markup.rs`): for 5,072 corpus formulas the port's markup equals KaTeX's
  `renderToString` output (MathML half removed) byte for byte, including error spans and
  internal errors: **5,070 / 5,072**. The two others are `adv-braces-200` / `-1000`, nested
  deeper than `parse_error::MAX_DEPTH` (100): internal error here (bounded native stack), listed
  in `tests/data/markup_known.txt`.
- **Layout** (`tests/layout.rs`) against KaTeX in Chromium (Electron 33, DSF 2, 16 px host):
  4,536 formulas compared (31 with system-font fallback glyphs and 1 that Chromium wrapped are
  excluded). 3,218 match exactly on Chromium's LayoutUnit grid (1/128 px); 4,367 (96.3 %) are
  within 0.02 px everywhere; every formula width matches. Glyphs: 26,436 compared, 70.8 % exact,
  97.6 % within 0.02 px (deviation in LayoutUnits: 0 → 70.8 %, 1 → 17.8 %, 2 → 9.0 %, 3 → 1.7 %,
  ≥4 → 0.7 %). Rules/borders 910/1,018 and SVG viewports 1,014/1,162 within 0.02 px (the rest
  0.023–0.06 px). Max deviation 0.047 px outside the adversarial set; 0.234 px for 30-deep
  nested subscripts (one LayoutUnit per level). The test asserts these bounds.
- **Adversarial** (`tests/adversarial.rs`): 10,000-deep braces, 2,000-deep fractions/scripts/
  `\left`, macro bombs, huge rules/kerns/raiseboxes, 3,000-cell matrices, control characters:
  all bounded (< 2 s in a debug build) and never crash.

## Layout model (what Chromium does with KaTeX's markup)

Calibrated against the harness (`tools/README.md` has the evidence):
- LayoutUnit = 1/(64·DSF) CSS px; specified lengths truncate (`LayoutUnit(float)`), relative
  offsets (`top`/`left`) round, text widths ceil, borders floor to device pixels (min 1).
- Shaping size = font size truncated to 1/64 device px; advances are `hmtx` × size (no kerning,
  ligatures or GSUB in KaTeX's fonts); adjacent text nodes are one run.
- Line boxes: Blink's integer ascent/descent (descent rounded down borrows a pixel), half-leading
  floored to whole pixels. `vlist-t` tables: auto column widths, bottom-aligned cells; a two-row
  table's baseline is the first row's bottom (`TWO_ROW_FUDGE` = 0 is the best global fit).
- Absolutely positioned boxes (SVGs, laps, tags, CD labels) resolve against the nearest
  positioned ancestor; inline-flex (laps) is a minimal flex model.
- `.eqn-num` counters restart at 1 per formula (Chromium's run across the whole page).

## For the painting crate

- Fonts: `Glyph::font` is one of 20 `KatexFont`s (`file_name()` in `assets/fonts/katex/`, OFL);
  load faces lazily. Draw `Glyph::ch` at (`x`, `y` = baseline) at `size` CSS px. Honour
  `synthetic_bold` / `synthetic_italic` (Chromium emboldens/skews; advances unchanged) and
  `fallback` (the face lacks the char: use a system font). `shadow` glyphs are `\pmb`'s offset copy.
- Rects: fill (`dashed` → dashed border line).
- Paths: `PathItem::paths` are SVG path strings in viewBox units; map with `transform`
  (`x' = sx·u + tx + item.x`, `y' = sy·v + ty + item.y`), fill non-zero, clip to `clip` when set
  (that is how `\sqrt`/stretchy arrows crop their 400em-wide paths). `lines` are `\cancel`
  strokes (endpoints and width in CSS px, relative to the viewport origin).
- Colours: `Color::Current` = surrounding text colour, `Color::Error` = the theme's error colour,
  `Color::Rgba(0xRRGGBBAA)`. Transparent content (phantoms) is already omitted.
- `Rendered::Error`: draw the raw TeX in the surrounding font in the error colour;
  `Rendered::Internal`: raw TeX as inline code (what MathTex.tsx does).
- Inline wrapping: `MathLayout::bases` are the unbreakable units (items `start..end`); Chromium
  may break between them. `lines` are `\\` lines; `tag_items` go at the container's right edge.

## Cost

- Typical formula (release, fat LTO): median 8 µs, p90 33 µs, p99 0.2 ms; worst corpus entry
  (500-term sum) 5.6 ms. `Item` is 32 bytes. One pass per box (memoized intrinsic widths).
- Release binary: +1.12 MB over an empty binary (fat LTO, stripped). About 0.38 MB is tables, of
  which ~0.2 MB is the symbol table's `&str` pointers and their relocations.
- Stack: nesting is bounded (`MAX_DEPTH` 100, `MAX_LAYOUT_DEPTH` 1200); the deepest accepted
  formulas fit in 1 MB of stack in release.

## Next steps

1. Binary size: store symbol/macro strings as offsets into one `&'static str` (saves ~0.2 MB);
   consider `opt-level = "s"` for this crate.
2. The remaining single-LayoutUnit vertical differences: Chromium moves inline-block content by
   one LayoutUnit depending on sub-unit values that do not change box sizes (see the calibration
   recipe below); `TWO_ROW_FUDGE` is a global fit, not the rule.
3. DSF 1 vectors (`tools/harness/run.sh … --dsf 1`) and a test at that scale.
4. A wrapping helper for callers (greedy line fill over `bases`, as Chromium does).

## Running things

- Tests: `cargo test -p decibell-tex` (`TEX_MARKUP_VERBOSE=1`, `TEX_LAYOUT_VERBOSE=1`,
  `TEX_LAYOUT_SHAPES=1`, `TEX_LAYOUT_FILTER=<id>`, `TEX_LAYOUT_DUMP=<file>` for per-glyph
  deviations). Debug-mode tests run on big-stack threads.
- Regenerate vectors: `tools/README.md` (`node tools/corpus/build_corpus.mjs`,
  `node tools/gen_markup.mjs`, `tools/harness/run.sh tools/corpus/corpus.jsonl tests/data/layout.jsonl.gz`).
- Calibrate: write a corpus JSONL, run the harness on it, then
  `TEX_LAYOUT_DATA=<out.jsonl.gz> TEX_LAYOUT_DUMP=<dump.tsv> cargo test -p decibell-tex --test layout -- --nocapture`.
- Debug one formula: `cargo run -p decibell-tex --example layout_dump -- 'x^2'` and
  `--example tex2markup -- --plain < lines.txt`; `--example bench -- tools/corpus/corpus.jsonl` (release).
- Regenerate the function name table after adding names:
  `UPDATE_TABLES=1 cargo test -p decibell-tex table_is_current`.
