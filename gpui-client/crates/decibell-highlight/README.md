# decibell-highlight

Colours fenced code blocks for the native client the way the Electron client's
`CodeBlock.tsx` does (lowlight 3.3.0 on highlight.js 11.11.2), without a
JavaScript engine: `(language, code) → spans`. No GPUI, no I/O.

```rust
let out = decibell_highlight::highlight(Some("rs"), code); // name or alias, any case
for span in &out.spans {            // consecutive byte ranges covering `code`
    draw(&code[span.range.clone()], span.token, span.italic);
}
```

## How it works

1. **Extractor** (`tools/extract.mjs`, Node, run by hand): reads which grammars
   `CodeBlock.tsx` registers (26 + plaintext, plus its `toml` alias), loads
   them from `electron-client/node_modules/highlight.js`, lets highlight.js
   compile them, and writes each compiled mode graph to `data/<lang>.hlg`
   (regex sources, group counts, keyword tables, scopes, relevance, flags,
   sub-language references, callback ids) and `src/generated.rs` (languages,
   aliases, scope and callback enums). It fails on an unknown mode field, an
   unknown function, a flag combination the port can't express, or a source
   file whose hash differs from `tools/pins.json`.
2. **Engine** (`src/engine.rs`): a port of highlight.js's `_highlight`,
   function for function: the resumable multi-regex, keyword processing,
   `beginScope` / `endScope`, `starts`, `endsWithParent` / `endsParent`,
   sub-languages (explicit, with continuations, and auto-detected by
   relevance), the zero-width and infinite-loop guards. `src/grammar.rs` joins
   a mode's regexes exactly as `_rewriteBackreferences` does and compiles them
   on first need.
3. **Callbacks** (`src/callbacks.rs`): the six functions the registered
   grammars attach to modes, hand-ported:

   | id | source | used by |
   |---|---|---|
   | `SkipIfHasPrecedingDot` | core.js, every `beginKeywords` mode | 16 grammars |
   | `ShebangAtStart` | `hljs.SHEBANG` `on:begin` | javascript, typescript, ruby, bash |
   | `SaveBeginMatch` / `EndSameAsBegin` | `hljs.END_SAME_AS_BEGIN` | c, cpp (raw strings), php, ruby, bash (heredocs) |
   | `SaveBeginMatch1Or2` | php `HEREDOC` `on:begin` | php |
   | `JsIsTrulyOpeningTag` | `XML_TAG.isTrulyOpeningTag` (JSX or generic?) | javascript, typescript |

4. **Regex engine** (`src/regex.rs`): [regress](https://github.com/ridiculousfish/regress)
   0.12.0, run over UTF-16 code units (UCS-2 view without the `u` flag, code
   points with it), as JavaScript does. A patched copy is vendored in
   `vendor/regress` (`vendor/regress-decibell.patch` is the whole difference):
   - a per-thread step / deadline budget for the backtracking matcher;
   - JavaScript's case-insensitive matching without the `u` flag (upstream
     used Unicode folding for classes, so `[a-z]` with `i` matched `ſ` and `K`);
   - a `few-properties` feature that keeps only `\p{L}`, `\p{XID_Start}`,
     `\p{XID_Continue}` (what the grammars use), saving about 550 KiB.

   Why regress: of the 990 distinct regexes in the grammars (220 with
   lookahead, 19 with backreferences), `regex` compiles 761 as written,
   `fancy-regex` 980 (and differs in `\w` / `\d`, `$`, case folding and
   UTF-16 semantics), regress all 990 with ECMAScript semantics.

## Output

`Highlighted.spans` are consecutive, non-empty byte ranges covering the text.
Each has the colour (`Token`) and `italic` the renderer's stylesheet would
give it, and `Highlighted::path(span)` is the full nesting (scopes and
sub-language wrappers, outermost first).

How nesting maps to colour: every rule in `globals.css`
(`.rich-codeblock .hljs-*`) has the same specificity and sets an inherited
property, so the innermost enclosing scope that the stylesheet styles wins;
`italic` is set anywhere inside a comment. The table is the same in all themes
(the palette variables differ per theme):

| `Token` | classes | palette variable |
|---|---|---|
| `Keyword` | keyword, built_in, literal, doctag, meta | `--color-accent-bright` |
| `Title` | title, name, section, type, selector-tag | `--color-accent` |
| `String` | string, regexp, quote, addition | `--color-success` |
| `Number` | number, symbol, attr, attribute, variable, template-variable, selector-class, selector-id | `--color-warning` |
| `Comment` | comment (italic) | `--color-text-muted` |
| `Deletion` | deletion | `--color-error` |
| `Operator` | operator, punctuation | `--color-text-secondary` |
| `Plain` | everything else | the block's text colour |

Scopes the grammars emit but the stylesheet leaves unstyled (they show the
enclosing colour): bullet, char, class, code, emphasis, function, link, params,
property, selector-attr, selector-pseudo, strong, subst, tag.

`Highlighted.language` is the resolved grammar (`None` = no registered
language named: plain text). The Electron header shows the name as typed.

## Limits

`Budget` (default: 256 KiB, 100 million regex steps, 500 ms). Past any limit
the block comes back as one plain span with `fallback` set. CodeBlock.tsx has
no cap of its own. The step count is deterministic; run highlighting off the
UI thread.

## Differences from lowlight, all deliberate

- **History dependence.** highlight.js's resumable matcher overwrites state
  shared between a mode's matchers, so on a long-lived instance a block can
  come out differently depending on what was highlighted before (30 of
  138,117 test cases). This crate always behaves like a fresh instance.
- **Where highlight.js's output is not the input.** For an empty `illegal`
  match it appends a literal `"\n"`; before a `\r` that replaces the character
  (12 fuzz cases, all CRLF). This crate keeps the input's text; the tree is
  the same.
- A span boundary inside a surrogate pair (JavaScript's `\\[\s\S]` before an
  emoji) snaps to the start of the character in the byte ranges.

## Tests

`cargo test -p decibell-highlight` needs no Node.

- `tests/differential.rs`: every row of `tests/vectors/*.tsv.gz` (lowlight's
  tree and relevance, from `tools/vectors.mjs`) must match exactly.
  `corpus.tsv.gz`: 317 cases (the samples in `tests/corpus/samples`,
  highlight.js's own `test/markup` and `test/detect` inputs, the chat
  fixture's code blocks, edge cases). `fuzz.tsv.gz`: 300 seeded mutations per
  grammar. `node tools/vectors.mjs --full` writes 5,000 per grammar to
  `tests/vectors/full/` (gitignored, hash in `tests/vectors/full.sha256`);
  the test runs it when present.
- `tests/engine.rs`: aliases, keywords, relevance, sub-language detection,
  spans, budgets, every regex compiles, the joined regexes equal
  highlight.js's own strings (`tests/vectors/joined.txt`), the colour table
  equals `globals.css`.
- `src/callbacks.rs`: each callback twin.

## Cost

`cargo run --release -p decibell-highlight --example measure` prints embedded
size, first-use time, memory and throughput per language.

## Upgrading highlight.js / lowlight, or changing the registered set

1. Update `electron-client` (or edit `CodeBlock.tsx`).
2. `node tools/extract.mjs` fails listing the changed files. Read the diff of
   `lib/core.js` (port engine changes to `src/engine.rs`) and of each grammar's
   functions. For a new callback, add it to `CALLBACKS` in the extractor and
   to `src/callbacks.rs` (the `match` there is exhaustive, so the build fails
   until it is ported).
3. `node tools/extract.mjs --update-pins`.
4. Update `TAG` / `SHA256` in `tools/fetch-fixtures.sh`, run it, then
   `node tools/vectors.mjs --full --update-hash`.
5. `cargo test -p decibell-highlight`; a new `\p{…}` escape needs an arm in
   the vendored regress's `few-properties` list.

## Licences

Grammar data derives from highlight.js 11.11.2 (BSD-3-Clause,
`LICENSE-highlight.js`). `vendor/regress` is MIT OR Apache-2.0.
