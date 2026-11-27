# gpui-client/assets

Third-party and renderer-derived assets for the GPUI client. One script owns all of them:

```sh
node gpui-client/tools/fetch-assets.mjs           # fetch what's missing or stale, refresh the derived files, verify
node gpui-client/tools/fetch-assets.mjs --check   # verify only: no network, no writes; exit 1 if a committed asset is stale
node gpui-client/tools/fetch-assets.mjs --print   # also print the sha256 of every pinned file (to update the pins)
```

Zero dependencies (Node ≥ 18). Every download is pinned: fonts by URL + SHA-256, npm tarballs by the SRI integrity in
`electron-client/package-lock.json` (so a renderer dependency bump moves the client with it), lucide-static by its own
pin, which the script refuses to use if it no longer matches the renderer's lucide-react version. A second run is a
no-op that only verifies. Run it after a fresh clone (for the gitignored folders) and after a renderer change to
fonts, sounds, lucide imports or inline SVG icons.

| Folder | In git? | What | Source | Version | Licence |
|---|---|---|---|---|---|
| `fonts/` | committed | Inter, Mona Sans (variable TTF), JetBrains Mono (4 static TTF) + one OFL text each | google/fonts @ `8b0a1d0`; fonts.gstatic.com `jetbrainsmono/v24` | Inter 4.001, Mona Sans 2.000, JetBrains Mono 2.211 | SIL OFL 1.1 |
| `icons/lucide/` | committed | the lucide icons the renderer imports, `manifest.json`, `LICENSE` | `lucide-static` npm tarball, names resolved through `lucide-react`'s exports | 1.14.0 (= the renderer's lucide-react) | ISC (+ MIT for Feather-derived icons, in `LICENSE`) |
| `icons/inline-svg-components.md` | committed, generated | inventory of the renderer's hand-written SVG icons | `electron-client/src` | — | — |
| `sounds/` | committed | the UI sounds, byte-identical copies | `electron-client/src/assets/sounds/` | — | the project's own |
| `twemoji/` | **gitignored**, fetched | 3,720 SVGs, `index.json`, `LICENSE-package.txt` | `@twemoji/svg` npm tarball | 15.0.0 | graphics CC-BY 4.0 (Twitter / jdecked Twemoji), package MIT |
| `emoji-data/` | **gitignored**, fetched | `native.json` (the picker's dataset), `LICENSE` | `@emoji-mart/data` npm tarball, its package `main` | 1.2.1 (Unicode 15 set) | MIT |
| `linux/decibell.desktop` | committed | the desktop entry packaging installs (`MimeType=x-scheme-handler/decibell`, `Exec=decibell %U`, `StartupWMClass=decibell`) | written for this client (`decibell-platform`) | — | the project's own |


The desktop entry is never installed or registered by anything here: on a development machine the installed Electron
client owns `decibell://`. Its id must stay `decibell` (the window's `app_id` / `WM_CLASS` and the launcher progress's
`application://decibell.desktop`); the file's comment says what packaging does with it.

CC-BY 4.0 requires attribution for the Twemoji graphics: "Twemoji by Twitter, Inc and other contributors, licensed under
CC-BY 4.0" belongs in the client's About / licences screen, as for the renderer.

## Fonts

The renderer imports fontsource packages (`@fontsource-variable/inter@5.2.8` via `index.css`,
`@fontsource-variable/mona-sans@5.3.0` via `standard.css`, `@fontsource/jetbrains-mono@5.3.0` 400/500/600/700). Those
are the Google Fonts API's builds cut into unicode-range `.woff2` subsets. GPUI reads neither WOFF2 nor split subsets,
so the client bundles the unsplit TTFs of the same builds:

| File | Bytes | Family (name ID 1 / ID 16) | Axes (min / default / max) | Named instances |
|---|---|---|---|---|
| `InterVariable.ttf` | 876,576 | `Inter` / — | `opsz` 14 / 14 / 32, `wght` 100 / 400 / 900 | Thin … Black (100–900 by 100), all at opsz 14 |
| `MonaSansVariable.ttf` | 349,140 | `Mona Sans ExtraLight` / `Mona Sans` | `wdth` 75 / 100 / 125, `wght` 200 / **200** / 900 | ExtraLight … Black (200–900 by 100), all at wdth 100 |
| `JetBrainsMono-Regular.ttf` | 112,172 | `JetBrains Mono` / — | static, usWeightClass 400 | — |
| `JetBrainsMono-Medium.ttf` | 112,204 | `JetBrains Mono Medium` / `JetBrains Mono` | static, 500 | — |
| `JetBrainsMono-SemiBold.ttf` | 112,160 | `JetBrains Mono SemiBold` / `JetBrains Mono` | static, 600 | — |
| `JetBrainsMono-Bold.ttf` | 112,092 | `JetBrains Mono` / — (style Bold) | static, 700 | — |

Family names to ask for: **`Inter`**, **`Mona Sans`**, **`JetBrains Mono`** (fontdb, DirectWrite and CoreText use the
typographic family, ID 16, where there is one).

Checked glyph for glyph (outline + advance, fontTools) against the renderer's subsets in `electron-client/node_modules`:
Inter, all 7 subsets (1,690 code points) at the default, latin also at wght 600: 0 differences; Mona Sans, all 3
subsets (501 code points) at the default, latin also at wght 600 / wdth 112.5: 0; JetBrains Mono, all 6 subsets of
each of 400 / 500 / 600 / 700 (729 code points per weight): 0.
Versions match too (`Version 4.001;git-66647c0bb`, `2.000`, `2.211`).

Why JetBrains Mono is four static faces, not the variable TTF from google/fonts: the renderer loads exactly these four
(any other weight snaps to one of them under CSS font matching), they are the files fontsource's static subsets are
cut from (0 differences, same feature set), and the variable TTF's named instances have **no SemiBold**, so a text
stack that only matches named instances (GPUI's DirectWrite path: `IDWriteFontSet::GetMatchingFonts`) would draw the
console themes' 600 as 700.

Fidelity notes for the UI layer:

- **Inter has no optical sizing in the renderer.** `@fontsource-variable/inter/index.css` is the wght-only cut (no
  `opsz` axis), so Chromium's `font-optical-sizing: auto` has nothing to drive and Inter renders at opsz 14 at every
  size. The bundled TTF has the axis with default 14: leave it there. Where a text stack sizes optically on its own
  (CoreText does for fonts with an `opsz` axis), pin `opsz` to 14. (`qt-client/HANDOFF.md` §2a says the opposite; it
  is wrong for this renderer.)
- **The renderer's `font-feature-settings: "cv11", "ss01", "ss03"` (body, `globals.css`) does nothing**: the Google
  Fonts subsets fontsource ships drop those features. The renderer's fonts carry only: Inter `calt ccmp dnom frac
  locl numr pnum tnum`; Mona Sans `ccmp dnom frac liga locl numr pnum tnum`; JetBrains Mono `calt ccmp frac locl`. The
  bundled google/fonts TTFs still have `cv01`–`cv14`, `ss01`–`ss08` and more, so **don't enable cv11 / ss01 / ss03**:
  that would change `a`, the digits and braces against the renderer. Default shaping (calt, liga, ccmp, locl) matches.
- **Mona Sans's default instance is ExtraLight** (wght 200): always ask for the weight explicitly (body text is 400).
- **Weights in use**: 400, 500, 600, 700 only (Tailwind `font-normal` / `medium` / `semibold` / `bold`, the themes'
  `--t-fw-emphasis`, one inline `fontWeight: 600`). Inter and Mona Sans have a named instance at each.
- **Widths**: Mona Sans is set at wdth 100 except the server tiles (`--t-tile-stretch`, 112.5 %) and the wordmark
  (`--t-wordmark-stretch`, 125 %) under nocturne / matinee / graphite*. The vendored GPUI's `Font` has family, weight,
  style and features only: cosmic-text (Linux) drives `wght` from the weight and leaves other axes at their defaults;
  DirectWrite (Windows) picks named instances, all at wdth 100. So 112.5 % / 125 % need either static instances (the
  Google Fonts API serves one per request, e.g. `css2?family=Mona+Sans:wdth,wght@112.5,600`, or fontTools'
  instancer) registered under their own family names, or axis support added to GPUI. Done with static
  instances: "Static faces" below.
- **Italics** are synthesised in the renderer (no italic face is loaded); bundle none.

Re-pinning: when the renderer bumps a fontsource package the script fails until `FONTSOURCE` in the script is updated.
Check what Google now serves (`metadata.json` `version`, the TTF's name ID 5), update the URLs / commit and the
SHA-256s (`--print`), and redo the glyph comparison above.

### Static faces (`fonts/static/`, committed, generated)

What the GPUI client actually loads. GPUI at the pin sets no variable-font axes and its font query has no width
(`docs/gpui-notes.md` §5.3), so `tools/gen-fonts.sh` cuts one static TTF per (family, width, weight) in use with
fontTools' instancer, and gives each width its own family name with ordinary weight classes inside:

| Family (name ID 1 / 16) | Cut from | Weights | Bytes each |
|---|---|---|---|
| `Decibell Inter` | `InterVariable.ttf` at `opsz` 14 | 400 500 600 700 | ~342,000 |
| `Decibell Sans` | `MonaSansVariable.ttf` at `wdth` 100 | 400 500 600 700 | ~101,000 |
| `Decibell Sans Plus` | … at `wdth` 112.5 (server tiles) | 400 500 600 700 | ~101,000 |
| `Decibell Sans Max` | … at `wdth` 125 (the wordmark) | 600 | ~101,000 |
| `Decibell Mono` | the four `JetBrainsMono-*.ttf`, renamed only | 400 500 600 700 | ~112,000 |

17 files, 2.73 MB. The families are renamed because Mona Sans reserves the font name "Mona" for unmodified copies
(`OFL-MonaSans.txt`; an instance is a Modified Version) and because an installed font of the same name would win ties
against the embedded one in GPUI's Linux matcher. Outlines, metrics and layout tables are those of the sources
(the `STAT` table is dropped, `OS/2` weight / `fsSelection` and the `name` table rewritten). The script is
deterministic: `tools/gen-fonts.sh --check` regenerates in memory and fails if a committed file differs; it also
writes `crates/decibell-ui/src/fonts_generated.rs`, the table the faces are registered and checked from. To add a
weight or width, edit `FAMILIES` in `tools/gen-fonts.py`. The same OFL 1.1 texts apply.

## Twemoji (`twemoji/`, fetched)

All 3,720 SVGs of `@twemoji/svg@15.0.0`, the set the renderer inlines (`scripts/build-twemoji-map.cjs` →
`Twemoji.tsx`). `index.json` lists every available sequence (`sequences`: file names without `.svg`, lower-case hex
code points joined by `-`).

Lookup rule — a parity decision for the owner: the renderer's `toCodePoints` drops U+FE0F from **every**
multi-code-point sequence, but Twemoji keeps it inside ZWJ sequences (`1f636-200d-1f32b-fe0f.svg`). Against the
picker's dataset that rule misses 124 of the 1,870 base emoji (😶‍🌫️, ❤️‍🔥, the gendered ZWJ people …) and 545 of the
1,525 skin-tone variants, which the renderer then draws with the OS emoji font. Twemoji's own rule (keep FE0F when the
sequence contains U+200D, drop it otherwise) misses only 1 base emoji (👁️‍🗨️). The comment in `Twemoji.tsx` attributes
the misses to "sequences twemoji 15.x retired"; they are the lookup's.

## Emoji data (`emoji-data/`, fetched)

`native.json` is `@emoji-mart/data@1.2.1`'s `sets/15/native.json`, which is what `import data from "@emoji-mart/data"`
loads in `EmojiPicker.tsx`, `MessageText.tsx` and `RichInput.tsx`. Shape:

```text
{ categories: [{ id, emojis: [emoji id …] }]       8: people nature foods activity places objects symbols flags
  emojis: { <id>: { id, name, keywords: [..], skins: [{ unified, native }], version } }   1,870 (305 with 6 skins)
  aliases: { <alias>: <id> }                        61, unused by the renderer
  sheet: { cols, rows } }                           sprite-sheet geometry, unused }
```

What the renderer uses: category order and members; `name`; `keywords`; `skins[0].native` only (there is no skin-tone
chooser); a client-side "Frequently used" category of the last 24 picked ids (`decibell.emoji.recent`). Search is the
lower-cased trimmed query as a substring of the id, the name or any keyword, in the dataset's key order, capped at 120.
`:shortcode:` (`/:([a-z0-9_+-]+):/gi`, lower-cased) expands to `emojis[id].skins[0].native` in messages and the composer.

Loading in the native client: embed or read `native.json` and parse it lazily on the first picker open or shortcode
(~430 KB, a few ms with serde_json). Keep the object's key order (serde_json's `preserve_order`, or deserialize the
`emojis` map into an ordered map): search results and their 120 cap follow it.

## Lucide (`icons/lucide/`)

The renderer imports two lucide icons, `Settings` and `UserPlus` (both in `features/chat/WelcomeState.tsx`, `size={15}`,
default stroke width 2, no `absoluteStrokeWidth`, so the stroke is 2 × 15 / 24 = 1.25 px). `manifest.json` maps every
import name to its file and lists each use. The SVGs are lucide-static's files verbatim (24 × 24 viewBox, `fill="none"`,
`stroke="currentColor"`, width 2, round caps and joins); the script checks each against lucide-react's icon node, which
is what the renderer draws. Everything else is hand-drawn: see `icons/inline-svg-components.md` (62 icon components and
150 more inline `<svg>` in 64 components; generated, so it follows the renderer).

## Sounds (`sounds/`)

Copies of `electron-client/src/assets/sounds/*.wav` (kept identical by the script; it fails `--check` on drift).
Uncompressed PCM WAV, mono, 16-bit; 424,344 bytes in all:

| File | Rate | Length |
|---|---|---|
| `call_end.wav` | 44.1 kHz | 0.28 s |
| `call_ring.wav` | 44.1 kHz | 2.00 s (looped while ringing) |
| `connect.wav` | 48 kHz | 0.25 s |
| `deafen.wav` / `undeafen.wav` | 48 kHz | 0.195 s |
| `disconnect.wav` | 48 kHz | 0.18 s |
| `mute.wav` / `unmute.wav` | 48 kHz | 0.10 s |
| `stream_start.wav` / `stream_stop.wav` | 48 kHz | 0.35 s |
| `user_join.wav` / `user_leave.wav` | 48 kHz | 0.30 s |

The renderer (`utils/sounds.ts`) decodes each once on first use (Web Audio resamples to the output rate), plays it one
shot at unity gain (overlapping plays mix), and loops `call_ring` until stopped. Decode once at startup, resample the
44.1 kHz pair to the device rate.

## Not here

The app icon (`electron-client/resources/icon.{png,ico,icns}`) is not copied; the packaging step can take it from
there. `electron-client/public/fonts/JetBrainsMono-{Regular,Medium}.ttf` are Tauri-era leftovers the renderer doesn't
load.
