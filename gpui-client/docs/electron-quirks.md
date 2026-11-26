# Electron behaviours found while porting

Porting each piece meant running the Electron client's own code against the Rust port, which surfaced
behaviours that look like bugs or accidents. The rule of the port is that **Electron defines behaviour**, so
unless a row says otherwise the native client mirrors it (with a test pinned to the real TypeScript), and
the owner decides: keep it in both clients, fix it in both, or let the native client differ.

"Where" names the Electron source; the Rust twin is in the crate named in the section title.

## Decisions waiting for the owner

| # | Behaviour | Where | Native client today |
|---|---|---|---|
| 1 | The active server tile's glow breathes forever; on Linux that makes the whole client composite at the display's refresh rate while visible (~15 % of a core, ~4,300 wakeups/s at 280 Hz) | `ServerBar.tsx` `ActiveTileGlow`, `globals.css` `@keyframes breathe` | **Differs**: breathes only while the window is focused, capped frame rate, present-only; rests when unfocused |
| 2 | A message with no formatting shows its raw text, so escape backslashes stay visible (`\*hi\*`) and an empty code fence shows its backticks | `MessageText.tsx:168-182`, `richText.ts:230` | Mirrors |
| 3 | 841 Twemoji images can never be shown (124 base emoji + skin variants): the lookup drops U+FE0F from every multi-code-point sequence, so those fall back to the system emoji font | `Twemoji.tsx:32-43` | Mirrors |
| 4 | Copying a selection drops emoji (they are inline SVG) and copies KaTeX's MathML for formulas | Chromium's selection of the rendered DOM | **Differs**: copies emoji as their characters and the TeX source (as the Qt spike did) |
| 5 | Link previews of pages declared as windows-1252 / latin1 / ascii decode as ISO-8859-1: curly quotes, dashes and € become invisible control characters | Node 20's `TextDecoder` in `linkPreview.ts` | Mirrors (one constant, `NODE20_LATIN1_DECODER`, flips it to what browsers do) |
| 6 | A failed attachment upload chunk is re-sent at the same offset, which can turn a half-written chunk into a run of 409s; abandoned uploads wait for the server's hourly sweep | `uploadAttachment.ts` | **Differs**: realigns with a `HEAD`, deletes abandoned uploads, retries the init rate limit |
| 7 | A sealed thumbnail fails to open when the server serves a different size than the one asked for (the nonce names the size) | `attachmentFetch.ts` | **Differs**: tries the other sizes |
| 8 | Reading a link's preview downloads up to 3 MiB of any body, including videos and direct images | `linkPreview.ts` | **Differs**: stops once the header that decides the result has arrived |
| 9 | Numbered equations (`\\tag`-less `equation` / `align`) count across the whole page in Chromium, so one message's formulas continue another's numbering | KaTeX's CSS counter | **Differs**: numbering restarts in each formula |
| 10 | TeX nested more than 100 levels deep | KaTeX renders it | **Differs**: shown as raw TeX (a bound so a pasted bomb can't overflow the stack) |
| 11 | Dates, times and counts are formatted by the language, not the regional formats: `Intl` / `toLocale*String(undefined, …)` take ICU's default locale, which on Linux comes from LC_ALL, else LC_MESSAGES, else LANG (measured in Electron 33; LC_TIME / LC_NUMERIC and `--lang` change nothing), so English with German formats still reads 11/7/2026, 2:32 PM, 1,234 | `lib/timeFormat.ts`, `MessageBubble.tsx` `DATE_FMT` / `DAY_FMT`, `DatePicker.tsx`, every `toLocaleString()` | **Differs**, by owner instruction (2026-11-26): the system's regional formats (Linux / macOS: the `LC_TIME` / `LC_NUMERIC` locale; Windows: the user's format settings) with the app's English words, C / POSIX as no preference (`decibell_model::regional`, CONVENTIONS "Regional formats") |

## Bugs in the Electron client itself (for `main`; not changed by this work)

- **No keyring → no sign-in on Linux.** `electron/main/addon.ts` passes `e2eeLocalKey: null` when `safeStorage`
  has no backend (or with `--password-store=basic`); napi accepts only `undefined` for an optional field, so
  `init` throws and the addon never starts. Fix: `?? undefined`. (Cannot happen in the native client: no napi.)
- The app re-points the `decibell://` handler at `decibell-electron-client.desktop` on every start.
- `stream_gpu_fallback` has a renderer listener but nothing emits it; `caps_refreshed` has neither.

## Mirrored quirks, by crate (full lists in each agent's notes; tests pin them)

### `decibell-richtext` (message grammar)
- Shortcodes expand before emoji matching, so an expansion can fuse with a neighbour (`🇦:flag-us:` → 🇦🇺 + a lone 🇸); an unknown `:id:` still loses its colons. The composer matches on the raw text instead.
- Past 10 code blocks / formulas in one message, the rest render as inline code, newlines included.
- An emphasis that renders nothing (`*```\\n```*`) still counts as formatting and blocks the lone-link rule.
- The reply / DM-list preview strips markers first, so it draws emoji inside code, math and URLs.
- Parser: closers are searched in the whole string, not the current range; a failed `$$` skips two characters; a failed double backtick doesn't retry as a single; the `_` word rule is ASCII-only (`é_a_é` italicises); multi-line inline code becomes a block.
- Autolinks: an emoji glued to a URL becomes part of the link (IDNA accepts it); Blink's URL parser quirks are emulated (seven, listed in the crate's tests).
- "Emoji-only" (jumbo) uses JavaScript's whitespace: U+FEFF counts, U+200B and U+0085 don't.

### `decibell-unfurl` (link previews, GIF search)
- A `secure_url` that doesn't resolve hides a valid `og:image`; a half-declared image size triggers a probe that replaces both and loses the declared one on failure.
- Any 3xx with a `Location` is followed (300 and 304 too); `Location` is read as latin1, so UTF-8 paths get double-encoded.
- oEmbed: `thumbnail_width` as a string is ignored, `1e21` becomes 1; the thumbnail resolves against the page URL; a blank title / provider gives an empty string with no hostname fallback.
- Tag scanning: `property=""` doesn't fall back to `name`; a `>` inside an attribute value cuts the tag; an SVG `<title>` and meta tags after `</head>` in the same chunk still count.
- GIF search: a `null` item fails the whole search; `zh-Hans-CN` sends KLIPY `locale=ha`; ids above 2^53 round as in JS.
- The preview cache keeps an expired entry's old position, so a refreshed entry is among the first evicted.

### `decibell-attachments`
- ThumbHash bytes can differ in the last bit between JS engines on perfectly symmetric pictures (`Math.cos` at exact ties); the port matches Electron's V8. Real photos differ between clients anyway because Chromium's canvas downscale isn't reproducible.

### `decibell-highlight`
- highlight.js's resumable matcher shares state between a mode's sub-matchers, so a long-lived instance can colour 0.02 % of inputs differently from a fresh one; the port always behaves like a fresh instance.
