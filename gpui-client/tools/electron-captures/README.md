# electron-captures: the Electron renderer, captured like the GPUI client

Renders the Electron client's **real** React components (imported from `electron-client/src`, not
copies) in a standalone page with the app's own `globals.css`, captures them with an offscreen
Electron window, and pairs each PNG with the GPUI client's capture of the same screen. Nothing talks
to a server or to native code: `window.decibell` is a stub. Electron, Vite, Tailwind and React all
come from `electron-client/node_modules`; nothing is installed.

The GPUI capture tool writes `<screen>-<theme>.png` at 1300 × 820: a 1280 × 800 window with a 10 px
transparent margin all round and a 32 px title bar, so its content area is `1280x768+10+42`. This
harness captures the same 1280 × 768 content area under the same names.

## Run

```sh
tools/electron-captures/run.sh                          # build + every page × all 7 themes
tools/electron-captures/run.sh --pages kit-scroll --themes nocturne,matinee
tools/electron-captures/run.sh --no-build --out /tmp/x  # reuse the last build
node tools/electron-captures/pairs.mjs                  # GPUI | Electron | difference page
tools/electron-captures/typecheck.sh                    # tsc over runtime/ + pages/ (strict)
```

(paths from `gpui-client/`.) Output: `gpui-client/target/electron-captures/<page>-<theme>.png`, plus
`<page>-<theme>.json` when the page's `setup()` returned data. `pairs.mjs` reads the GPUI captures from
`gpui-client/target-kit/captures` (`--gpui <dir>`), the Electron ones from the default out dir
(`--electron <dir>`) and writes `target/electron-captures/pairs/index.html` (`--out <dir>`), a
self-contained page (images copied next to it) with, per screen and theme, the GPUI crop, the Electron
capture, `magick compare` highlighting in red the pixels that differ by more than `--fuzz` (default
2 %), and their count. A GPUI capture of another size W × H is cropped `(W−20)x(H−52)+10+42`.

`run.sh` options: `--pages a,b` (default: every page), `--themes …` (default: nocturne, matinee,
graphite, graphite-light, console, console-light, console-split), `--out <dir>`, `--query k=v` (added
to every page URL; pages read it with `URLSearchParams`), `--no-build`. Environment:
`CAPTURE_DPR` (default 1; the PNG is then the size × DPR), `CAPTURE_TIMEOUT_S` (whole run, default 300),
`CAPTURE_GRAYSCALE_TEXT=1` (see gotchas). A run of one page in seven themes takes about 2 s after the build.

## Add a page

One file in `pages/`, named after the GPUI screen it mirrors (`kit-forms.tsx` → `kit-forms-<theme>.png`).
It is picked up automatically (files starting with `_` are helpers, not pages):

```tsx
// pages/kit-forms.tsx
import { useState } from "react";
import Switch from "@app/components/Switch";                  // @app = electron-client/src
import { $, centre, click } from "../runtime/harness";

export default function KitForms() {                           // fills the 1280 × 768 viewport
  const [on, setOn] = useState(false);
  return <div className="p-4"><Switch checked={on} onToggle={() => setOn(!on)} label="demo" /></div>;
}
export async function setup() {                                // optional, awaited before the capture
  await click(...centre($("[role=switch]")));                  // a real click (sendInputEvent)
  return { checked: $("[role=switch]").getAttribute("aria-checked") }; // → kit-forms-<theme>.json
}
export const size = { width: 1280, height: 768 };              // optional (this is the default)
```

Then `run.sh --pages kit-forms`. If a page needs app state, seed the stores before rendering
(`useSomeStore.setState({...})` at module top level or in `setup()`), never through the stub.

**Pages drawn as the whole window.** Anything that places itself against the window (a modal centred in
it, toasts 24 px from its top, a menu clamped to it) must see the same window as in the app, title bar
included. Draw such a page in `Framed` (`pages/_frame.tsx`: a 32 px title-bar band over the content),
export `size = { width: 1280, height: 800 }` (or the GPUI capture's `--size`), and return `{ framed: true }`
from `setup()`; `pairs.mjs` then crops the band off before comparing (`kit-overlays`, `kit-dialogs`,
`kit-settings`, `kit-settings-rail`).

**The kit pages** (`kit-*.tsx`) mirror `crates/decibell-ui/src/kit.rs` screen for screen with the same
layout helpers (`Page`, `Section`, `Row` in `_frame.tsx` = the kit's `page`, `section`, `row`). Change a
kit screen and its page together. Capture both sides, then pair:

```sh
cargo run -p decibell-app --features capture --bin decibell-capture -- --screen kit-forms --all-themes \
    --out-dir target-kit/captures            # kit-settings-rail: --size 900x800
CAPTURE_GRAYSCALE_TEXT=1 tools/electron-captures/run.sh --pages kit-forms
node tools/electron-captures/pairs.mjs     # → target/electron-captures/pairs/index.html
```

`CAPTURE_GRAYSCALE_TEXT=1` for comparisons: GPUI draws grayscale text, and LCD fringes would otherwise be
most of the difference.

**Icons.** `pages/icons-dump.tsx` is not a screen: it renders the renderer's glyph components to SVG
(`renderToStaticMarkup` of the real components, plus the inline `<svg>` of a few local ones and of the
settings tabs) and returns them from `setup()`; `icons/extract.sh` runs it and writes
`gpui-client/assets/icons/<group>/<name>.svg` (`icons/extract.mjs`). Add a component there to add an icon.

## The shell pages (`main-server`, `main-server-narrow`, `main-home`, `auth`, `auth-register`)

`main-*` render the renderer's real `MainLayout` with App.tsx's event hooks and feed them the GPUI shell's
fixture (`crates/decibell-shell/src/fixture.rs`): the same core events, as the addon would deliver them,
through the bridge's `listen` (`pages/_shell.tsx` keeps the handlers), then the fixture's navigation.
Regenerate the JSON after changing the fixture:

```sh
cargo run -p decibell-shell --features fixtures --bin decibell-shell-fixture -- tools/electron-captures/pages/fixtures
cargo run -p decibell-app --features capture --bin decibell-capture -- --screen main-server --all-themes --out-dir <gpui>
# main-server-narrow: --size 900x700 into another dir, files renamed main-server-narrow-<theme>.png
CAPTURE_GRAYSCALE_TEXT=1 tools/electron-captures/run.sh --pages main-server,main-server-narrow,main-home,auth,auth-register
node tools/electron-captures/pairs.mjs --gpui <gpui> --electron target/electron-captures
```

## The chat pages (`chat-fixture`, `chat-features`)

They render the renderer's real `ChatPanel` (so `RealMessageList`, `MessageBubble` and everything under
them) on the GPUI chat fixtures (`crates/decibell-chat/src/fixture.rs`: the 10,000-message channel of
`tools/dev/fixture.py`, its 150 newest rows; and a hand-written channel with every row type), replayed
like the shell's (`pages/_chat.tsx`). `setup()` returns every row's `offsetTop` / `offsetHeight` and
fractional box, and what `Selection.toString()` gives for a selection across five rows.
`chat-rows.mjs` compares the rows with the GPUI list's own dump, by message id:

```sh
cargo run -p decibell-chat --features fixtures --bin decibell-chat-fixture -- tools/electron-captures/pages/fixtures
CAPTURE_GRAYSCALE_TEXT=1 tools/electron-captures/run.sh --pages chat-fixture,chat-features --out <electron>
DECIBELL_CHAT_ROWS_DIR=<gpui> decibell --theme <theme> --selftest chat-rows     # per theme
node tools/electron-captures/chat-rows.mjs <gpui> <electron> <theme>             # exits 1 on a height difference
```

The composer in the Electron capture is the real one; the GPUI screens draw an empty card of its resting
size (the composer is another crate's), so the list has the same 696 px in both.

## The voice pages (`voice-*`, `go-live*`, `call-*`)

They render the renderer's real `MainLayout` (VoicePanel, StreamViewPanel, MiniStreamPlayer, IncomingCallModal,
CallStage inside DmChatPanel) on the GPUI voice fixtures (`crates/decibell-voice/src/fixture.rs`): the shell's events
and the scene's (Lounge's roster, the live streams, the MLS state, a ping, alice talking) through the event hooks, then
what the GPUI fixture does as the user (`pages/_voice.tsx`: posters, watches, focus, the loaded stream, the view, the Go
Live dialog — the real `CaptureSourcePicker`, opened by the page because it is VoicePanel's component state — and a call
in `callStore`). Drawn as the whole window (`Framed`, 1280 × 800): the dialog, the incoming card and the mini-player
place themselves against it. Go Live takes the native-encoder path (a canned `probe_native_encoders` answer), as the GPUI
client only has that one, so "Share sound" is offered.

```sh
cargo run -p decibell-voice --features fixtures --bin decibell-voice-fixture -- tools/electron-captures/pages/fixtures
cargo build -p decibell-app --features capture --bin decibell-capture
for s in voice-stage-1 voice-stage-4 voice-stage-9 voice-stage-25 voice-stage-live voice-stage-stream voice-stage-streams \
         voice-focus voice-mini go-live go-live-sharp go-live-slides go-live-saver go-live-advanced go-live-software \
         call-incoming call-stage; do
  target/debug/decibell-capture --screen $s --all-themes --out-dir target/captures-voice
done
CAPTURE_GRAYSCALE_TEXT=1 tools/electron-captures/run.sh --pages voice-stage-1,voice-stage-4,voice-stage-9,voice-stage-25,\
voice-stage-live,voice-stage-stream,voice-stage-streams,voice-focus,voice-mini,go-live,go-live-sharp,go-live-slides,\
go-live-saver,go-live-advanced,go-live-software,call-incoming,call-stage --out <electron dir>
node tools/electron-captures/pairs.mjs --gpui target/captures-voice --electron <electron dir> --out target/electron-captures/pairs-voice
```

Known differences that are not bugs: video (both draw the player's first state, a spinner on black; the pictures
themselves are never compared), the mini-player's label (the renderer prints the streamer's uid; GPUI their name), the
call screens' surroundings (GPUI's `call-stage` hosts the stage in a stand-in DM pane: no DM header, no friends rail), and
the panes other crates own (chat, members, search). In a worktree, `electron-client/node_modules` and the generated
`src/components/emoji/twemoji-data.json` are not checked out: link / copy them from the main checkout first.

## Conventions

- **Viewport** 1280 × 768 CSS px at DPR 1 = the GPUI window's content area. `export const size` overrides
  it; the runner sizes the window before loading the page.
- **Page module**: default export = the React component (mounted in `#root`, the size of the viewport,
  on the app's own `body` styles: `bg-bg-primary text-text-primary font-sans`). Optional
  `async setup()` runs after mount and fonts; its return value (JSON-able) is written next to the PNG.
- **Ready**: `window.__ready` resolves after fonts (Inter Variable, Mona Sans Variable, JetBrains Mono,
  400–700) + mount + `setup()` + `document.fonts.ready` + two animation frames, and waits (after mount
  and again after `setup()`, at most 3 s) for running CSS animations / transitions that end, so
  entrance animations and toggles are captured settled; infinite ones (spinners) are captured wherever
  they are. The runner then forces a repaint and captures. `localStorage` is cleared first (it persists across loads); the theme is set as
  `<html data-theme>`, as the app does.
- **The stub** (`runtime/stub.ts`, installed before any app module loads): every `window.decibell`
  method resolves `null`; `subscribe` / `on*` return `() => {}`; `listen` returns an unsubscribe
  function that is also awaitable (the real one is async). Calls are recorded in `window.__calls` as
  `{ method, args }`: `invoke("play_sound", …)` as `playSound` (camel-cased like `lib/ipc`), other
  bridge calls by path (`dialog.open`). Canned answers: `respond("getSettings", {...})` or
  `respond("gifs.status", (…args) => …)` from `runtime/harness`.
- **Input** (`runtime/harness.ts`): DOM `el.click()` / `dispatchEvent` is enough for React handlers.
  When Chromium itself must see the pointer (`:hover`, scrollbar hover, native drags) use `hover(x, y)`,
  `click(x, y)`, `drag(a, b)` or `input([...])`: real events through `webContents.sendInputEvent`
  (preload `window.__harness.input` → ipc → main). Coordinates are page CSS px.
- **Names** match the GPUI screens; one PNG per page × theme, no other files in the out dir except the
  JSON reports and `pairs/`.

## Files

| File | What |
|---|---|
| `run.sh` | build + shoot, the entry point |
| `build.sh`, `build.mjs` | `.work/` (gitignored): `node_modules` links, `app.css` (`@import` globals.css + `@source` lines), Vite build to `.work/dist` through Vite's JS API (no config file: one would be bundled into `electron-client/node_modules/.vite-temp`) |
| `runtime/` | `index.html`, `main.tsx` (page loader, fonts, `__ready`), `stub.ts`, `harness.ts` (helpers for pages) |
| `shoot.cjs`, `preload.cjs` | the offscreen Electron runner and its input bridge |
| `pairs.mjs` | the comparison page (Node, no deps, ImageMagick 7) |
| `typecheck.sh` | tsc with the renderer's options; its config lives in `.work/` under a non-default name |
| `pages/` | one file per screen |
| `pages/regional-stamps.tsx` | not a screen: the renderer's message times (`MessageBubble` / `DayDivider`) for a table of times, written to `crates/decibell-model/src/regional/renderer_stamps.json` (its header says how) |

## Gotchas (from electron-client/HANDOFF.md §5 "UI preview harness" and this harness)

- **Dates, times and counts are en-US**: `run.sh` exports `LC_ALL=en_US.UTF-8`, because the renderer's `Intl`
  takes ICU's default locale from LC_ALL / LC_MESSAGES / LANG (`--lang` doesn't change it); the GPUI captures
  pin `Regional::en_us()` to match. Running `shoot.cjs` by hand: set it yourself.
- Absolute paths everywhere: a relative out dir plus a `cd` once wrote the bundle elsewhere and a stale
  one kept loading. Don't put a `tsconfig.json` in the work dir or the harness (Vite then follows its
  references and fails on a missing `tsconfig.node.json`).
- Without the `@source` lines Tailwind leaves out the classes used in `electron-client/src` and `pages/`.
- One offscreen window is reused for every job: creating a second after destroying the first can fail
  with `ERR_FAILED (-2)`; loads are retried.
- Electron 33 logs a harmless `No component available` unhandled rejection at startup: logged, ignored.
  Fontconfig warnings about `49-sansserif.conf` are the system's, also harmless.
- Offscreen `el.focus()` fires no focus events: `focusIn(el)` dispatches `focusin`. Drags need
  `modifiers: ["leftButtonDown"]` on the moves (`drag` does it). `sendInputEvent` wheel deltas have the
  opposite sign to the DOM's (`deltaY: -120` scrolls down).
- The profile is `.work/profile` (`--user-data-dir` + `app.setPath`), never the app's. Never launch the
  installed Decibell, and never `pkill -f` with a pattern that matches your own command line.
- **Text is LCD-subpixel antialiased** (fontconfig's RGB setting, as in the real app on this machine:
  `-webkit-font-smoothing` does nothing on Linux). GPUI's text is grayscale, so every glyph edge shows
  in the difference. `CAPTURE_GRAYSCALE_TEXT=1` (Chromium's `--disable-lcd-text`) captures grayscale
  text for those comparisons; nothing else changes.

## Measured: Chromium's scrollbar under globals.css (`pages/kit-scroll.tsx`)

Electron 33.4.11 / Chromium 130.0.6723.191 on Linux, 2026-11-26. Repeat with
`run.sh --pages kit-scroll` (the JSON has each box's metrics; `--query hover=thumb|track|content|none|ethumb`
and `--query press=1` move or press the pointer).

- **The standard properties win.** `* { scrollbar-width: thin; scrollbar-color: var(--t-scrollbar)
  transparent }` sets non-auto values on every element, so Chromium ignores every `::-webkit-scrollbar*`
  rule in globals.css (the 6 px width, the 3 px radius and the `--t-scrollbar-hover` colour are dead
  CSS). Computed: `scrollbarWidth: "thin"`, `scrollbarColor: "rgba(232, 224, 248, 0.09) rgba(0, 0, 0, 0)"`
  (nocturne). Only an element that sets both back to `auto` gets the 6 px rounded `-webkit` bar (box E).
  `scrollbar-thin` (`SETTINGS_PANE` and others) is defined nowhere: a no-op class.
- **Geometry at scale 1** (vertical bar, box H px tall): 10 px thick, **takes layout space**
  (`offsetWidth − clientWidth = 10`); the track is transparent (nothing painted). Each end holds a 10 ×
  10 arrow zone with no background, so the track is `L = H − 20`. The thumb is 6 px wide with a 2 px gap
  each side (`bar.x + 2 … bar.x + 7`), square ends, no antialiasing. Length
  `max(round(L × clientHeight / scrollHeight), 20)`, offset from the track start
  `round((L − length) × scrollTop / (scrollHeight − clientHeight))`. A (400 px box, 1280 content,
  scrollTop 200): length round(118.75) = 119, top = 10 + round(59.3) = 69, i.e. rows 69–187 of the box.
- **Arrows**: crisp triangles in the thumb colour, always drawn (also at the scroll limits). Scale 1:
  up = row 5 (2 px, `bar.x + 4…5`) over row 6 (4 px, `bar.x + 3…6`); down = mirror, row H−7 (4 px)
  over row H−6 (2 px).
- **Colour**: the thumb is `--t-scrollbar` composited over what is behind the bar (here `--t-chrome`):
  nocturne `#232027`, matinee `#c7c3cd`, graphite `#212428`, graphite-light `#c8cbd0`, console `#1a1b1d`,
  console-light / console-split `#c4c8c6` (each within 1/255 of the alpha blend).
- **No hover or pressed state**: thumb hovered, track hovered, content hovered, pointer outside and
  thumb pressed give identical pixels (0 px differ); no thickening. (Hover does reach scrollbars
  offscreen: E's `-webkit` thumb turns `--t-scrollbar-hover`.)
- `scrollbar-gutter: stable` without overflow reserves the 10 px and paints nothing.
- **Other scales** (physical px; thickness = floor(10 × s)): 1.25 → 12 px bar, 8 px thumb;
  1.5 → 15 / 11; 2 → 20 / 16. The side gap stays 2 physical px, the arrow zones stay square
  (thickness × thickness), the minimum length is 20 DIP (30 px at 1.5, 40 at 2). Arrow rows from the end:
  1.25 → rows 5–7, widths 2 / 4 / 6; 1.5 → rows 6–9, widths 1 / 3 / 5 / 7; 2 → rows 8–12, widths
  2 / 4 / 6 / 8 / 10.
