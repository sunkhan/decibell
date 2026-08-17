# Handoff: Theme switcher (Appearance settings tab) + media audio player parity

## Overview
Decibell currently ships one hard-coded dark palette. This handoff adds **five selectable themes**, a new **Appearance** tab in the settings modal to switch between them at runtime, persistence of the choice across restarts, and two missing controls on the media **audio attachment player** (volume cluster + download button) so the mock matches the shipped component.

Nothing about the app's layout changes. Element order, positions, sizes and panel widths in `MainLayout` stay exactly as they are — this is a palette / typography / states change plus one new settings tab.

## About the design files
`Chat Polish.dc.html` in this bundle is a **design reference written as HTML**, not production code. It is a static, inline-styled recreation of the main server-chat screen in each theme so you can read exact values off it. Do **not** port its markup.

The task is to recreate these designs inside the existing `electron-client` React + Tailwind v4 codebase, using its established patterns: the `@theme` token block in `src/styles/globals.css`, Tailwind utility classes on components, zustand stores in `src/stores`, and the `invoke()` IPC bridge in `src/lib/ipc.ts`. Every component listed below already exists — you are re-tokenising them, not rewriting them.

Open the HTML file in a browser to see all five themes side by side. Each option carries a visible id badge (`1a`, `1c`, `1d`, `2a`, `2b`) matching the theme ids used throughout this document.

## Fidelity
**High fidelity.** Colors, type sizes, weights, letter-spacing, radii and spacing are final and are listed exactly below. Two caveats:

- Avatars in the mock are letter placeholders; the app keeps `UserAvatar` / `LetterAvatar` unchanged.
- The video attachment is a striped placeholder; the real `AttachmentList` video tile is unchanged apart from token colors.

## The five themes

| id | Name | Mode | Character |
| --- | --- | --- | --- |
| `graphite` | Graphite | dark | Current Decibell, regraded. Blue-tinted surfaces replaced by neutral-cool graphite so panels separate by value, not by hue. Mono metadata. |
| `graphite-light` | Graphite Light | light | Same ramp inverted; accent darkened for contrast on white. |
| `console` | Console | dark | Near-black, single mint signal accent, mono chrome, denser rows, 4px radii. |
| `console-light` | Console Light | light | Paper canvas, ink mono, mint compressed to a deep signal green. |
| `console-split` | Console Split | light | `console` chrome (titlebar, server bar, DM rail, channel sidebar, voice panel) with `console-light` message canvas and member list. |

`console-split` is not a third palette: it is `console` for chrome roles and `console-light` for content roles. Implement it as a composition of the two token sets, not a hand-authored third table.

---

## Token architecture (do this first)

`src/styles/globals.css` currently hard-codes values into Tailwind v4's `@theme` block. Keep every token **name** — all existing utility classes (`bg-bg-dark`, `text-text-muted`, `border-border`, …) must keep working untouched. Make the values indirect:

```css
@theme {
  --color-bg-darkest: var(--t-chrome);
  --color-bg-dark: var(--t-sidebar);
  --color-bg-mid: var(--t-content);
  --color-bg-light: var(--t-raised);
  --color-bg-lighter: var(--t-high);
  --color-bg-surface: var(--t-surface);
  --color-bg-titlebar: var(--t-chrome);
  --color-bg-dmbar: var(--t-chrome);
  --color-bg-primary: var(--t-chrome);
  --color-bg-secondary: var(--t-sidebar);
  --color-bg-tertiary: var(--t-sidebar);
  --color-bg-input: var(--t-input);
  --color-border: var(--t-border);
  --color-border-divider: var(--t-divider);
  --color-accent: var(--t-accent);
  /* …and so on for every existing token… */
}

:root, :root[data-theme="graphite"] { /* graphite values, see table */ }
:root[data-theme="graphite-light"] { /* … */ }
:root[data-theme="console"] { /* … */ }
:root[data-theme="console-light"] { /* … */ }
:root[data-theme="console-split"] { /* … */ }
```

Tailwind v4 compiles `@theme` entries to CSS custom properties, so the indirection resolves at runtime and switching `document.documentElement.dataset.theme` re-paints the whole app with no re-render and no class churn.

Three notes:

1. **`--color-bg-tertiary` is a real dependency.** `MembersList` draws status-dot borders with `border-bg-tertiary`; that value must equal the member-list background in every theme or the dots get a visible halo. Same for the `border-bg-primary` ring on `.custom-slider` thumbs.
2. **Fonts are per-theme too.** Add `--t-font-ui`, `--t-font-meta`, `--t-font-channel`, `--t-font-display` and point `--font-sans` / `--font-mono` / `--font-channel` / `--font-display` at them. `console*` themes set `--t-font-channel` and `--t-font-meta` to JetBrains Mono; `graphite*` set `--t-font-meta` to IBM Plex Mono and everything else to Inter.
3. **Radii are per-theme.** `console*` is a 3/4/4/8 scale; `graphite*` is 6/10/12/20. Route `--radius-sm|md|lg|xl` through `--t-radius-*`.

`IBM Plex Mono` is a new dependency (`@fontsource/ibm-plex-mono`); `Inter` and `JetBrains Mono` are already vendored. `Outfit` is no longer used by any theme — `--font-display` maps to the UI font.

### Foreground contrast rule (non-negotiable)
`globals.css` already states the rule: 11–12px metadata must clear ~4.5:1 on the surface it sits on. The light themes are where this bites — a mechanically inverted muted ramp lands around 2.4–2.9:1. The `--t-text-muted` / `--t-text-faint` values in the tables below are the *corrected* ones; use them verbatim rather than deriving light values from the dark ones. Verify with a contrast checker after wiring each theme, specifically: section labels (`ONLINE — n`, `TEXT CHANNELS`), message timestamps, the composer placeholder, member counts, the `DECIBELL` wordmark, and channel `#` glyphs. The composer placeholder is the weakest link in every theme: use `text-muted`, never `text-faint`. `text-faint` is reserved for non-textual use (inactive dots, dividers); any text that currently reaches for it — the `DECIBELL` wordmark included — uses `text-muted` instead, which is why the tables give `muted` and `faint` the same value for text purposes.

Also: de-emphasise offline member rows with a **color step**, not `opacity: 0.5`. Opacity multiplies against an already-muted token and drops the rows to ~1.9:1. Do not put an opacity multiplier on the row at all: at any value it drags the name below 4.5:1 on light paper. Render offline names in `text-muted` at full opacity and apply `opacity: .62` to the avatar tile only — that carries the visual step without touching text legibility. The same applies to inactive voice-channel dots: give them a real `text-faint` value rather than a dimmed `text-muted`.

---

## Token tables

Roles map to the existing token names as in the `@theme` snippet above. `on-accent` is the foreground used on filled accent surfaces (send button, play button, active tile).

### `graphite` (dark)
| Role | Value |
| --- | --- |
| chrome (titlebar, server bar, DM rail) | `#0e1116` |
| sidebar (channel + member panels) | `#14181f` |
| content (chat viewport) | `#191e26` |
| raised (cards, composer, popovers) | `#222834` |
| high (hover on raised) | `#2b3240` |
| surface | `#333b4c` |
| input | `#14181f` |
| border | `rgba(255,255,255,.07)` |
| border-divider | `rgba(255,255,255,.05)` |
| surface-hover / surface-active | `rgba(255,255,255,.045)` / `rgba(255,255,255,.08)` |
| accent / bright / hover / pressed | `#4f8cff` / `#8fb4ff` / `#3d7bef` / `#2f66cf` |
| accent-soft / accent-mid | `rgba(79,140,255,.13)` / `rgba(79,140,255,.20)` |
| on-accent | `#ffffff` |
| success / warning / error | `#4ec97a` / `#dca64a` / `#f2655c` |
| text primary / bright | `#e4e8ef` / `#f2f5f9` |
| text secondary / muted / faint | `#98a2b3` / `#8792a5` / `#8792a5` |
| radius sm/md/lg/xl | `6` / `10` / `12` / `20` px |
| font ui / meta | Inter (450 body, 500 labels, 600 emphasis) / IBM Plex Mono |

Sender-name colors stay driven by `stringToColor`, but re-seed its palette to this ramp: the mock uses `#8fb4ff` (sunkhan) and `#e08a8a` (Fiary) — mid-lightness, low-chroma versions of the current hues.

### `graphite-light`
| Role | Value |
| --- | --- |
| chrome | `#eceef2` |
| sidebar | `#f4f5f8` |
| content | `#fbfbfd` |
| raised | `#ffffff` |
| high | `#f1f3f6` |
| surface | `#e7eaef` |
| input | `#ffffff` |
| border | `rgba(15,20,30,.09)` |
| border-divider | `rgba(15,20,30,.07)` |
| surface-hover / surface-active | `rgba(15,20,30,.04)` / `rgba(15,20,30,.07)` |
| accent / bright / hover / pressed | `#2f6fe0` / `#1f57bd` / `#2864cf` / `#1f52ab` |
| accent-soft / accent-mid | `rgba(47,111,224,.11)` / `rgba(47,111,224,.18)` |
| on-accent | `#ffffff` |
| success / warning / error | `#22a75a` / `#b07d1e` / `#d54238` |
| text primary / bright | `#171b22` / `#12161d` |
| text secondary / muted / faint | `#5b6474` / `#5f6877` / `#5f6877` |
| radius, fonts | same as `graphite` |

Raised surfaces in light mode need a shadow, not just a border: `--shadow-raised: 0 1px 3px rgba(15,20,30,.10), inset 0 0 0 1px rgba(15,20,30,.07)`; float `0 8px 24px rgba(15,20,30,.14)`; modal `0 24px 64px rgba(15,20,30,.22)`. Avatar placeholder fills also need darkening (`#93a0b3`, `#2f6fe0`, `#7a4fd0`, `#5c8a6b`, `#a4707f`, `#b08a3c` with white initials).

### `console` (dark)
| Role | Value |
| --- | --- |
| chrome | `#07080a` |
| sidebar | `#0c0e11` |
| content | `#101215` |
| raised | `#17191b` |
| high | `#1e2124` |
| surface | `#23272a` |
| input | `#0c0e11` |
| border | `rgba(255,255,255,.07)` |
| border-divider | `rgba(255,255,255,.06)` |
| surface-hover / surface-active | `rgba(255,255,255,.03)` / `rgba(255,255,255,.06)` |
| accent / bright | `#7ef0a8` / `#a8f7c4` |
| accent-soft / accent-mid | `rgba(126,240,168,.10)` / `rgba(126,240,168,.25)` |
| on-accent | `#07120c` |
| success / error | `#7ef0a8` / `#f0786e` |
| text primary / bright | `#dfe4e2` / `#eef4f1` |
| text secondary / muted / faint | `#8b958f` / `#869089` / `#7f8a84` |
| radius sm/md/lg/xl | `3` / `4` / `4` / `8` px |
| font ui / meta / channel | Inter (message bodies only) / JetBrains Mono / JetBrains Mono |

`console` specifics: channel names, member names, timestamps, section labels, the titlebar title, the server-tile label and the composer placeholder are all **JetBrains Mono**; only message body text is Inter. Section labels are shortened (`TEXT`, `VOICE`) with `letter-spacing: .16em`. Active channel row = `bg-bg-light` + `box-shadow: inset 2px 0 0 var(--color-accent)`. Voice channels gain a right-aligned occupancy count (`2/8`, `0/8`) in 9px mono. The active server tile and the connected-voice dot get a mint glow: `box-shadow: 0 0 8px #7ef0a8`. The chat header gains a right-aligned `13 MEMBERS` count in 9.5px mono.

### `console-light`
| Role | Value |
| --- | --- |
| chrome | `#e7eae8` |
| sidebar | `#eff1ef` |
| content | `#f9faf9` |
| raised | `#ffffff` |
| high | `#d5dcd8` |
| surface | `#e9ebe9` |
| input | `#eff1ef` |
| border | `rgba(10,25,18,.11)` |
| border-divider | `rgba(10,25,18,.10)` |
| surface-hover / surface-active | `rgba(10,25,18,.045)` / `rgba(10,25,18,.08)` |
| accent / bright | `#0f7a45` / `#0b6238` |
| accent-soft / accent-mid | `rgba(15,122,69,.09)` / `rgba(15,122,69,.25)` |
| on-accent | `#ffffff` |
| success / error | `#0f7a45` / `#c8493d` |
| text primary / bright | `#161c19` / `#0c1310` |
| text secondary / muted / faint | `#5a665f` / `#5c6863` / `#616d67` |
| radius, fonts, mono discipline | same as `console` |

The mint glow does not survive on paper — replace `0 0 8px #7ef0a8` with `0 0 0 3px rgba(15,122,69,.16)`. Avatar tints: `#d5dcd8`/`#3f4a45`, `#0f7a45`/`#eafff2`, `#ddd6f6`/`#443a72`, `#f3dce4`/`#78384e`, `#efe9cf`/`#655519`.

### `console-split`
Chrome roles (`chrome`, `sidebar`, `input`, and the voice panel's raised surface) take **`console`** values; content roles (`content`, `raised`, `high`, `surface`, all text tokens, borders inside the chat area and member list) take **`console-light`** values. Accent is `#7ef0a8` on dark chrome and `#0f7a45` on light content — implement as two tokens (`--t-accent` for content, `--t-accent-chrome` for chrome) rather than one.

---

## Screen: Appearance settings tab

### Purpose
Let the user pick one of the five themes and see the change immediately.

### Placement
`src/features/settings/SettingsModal.tsx` — add to the `TABS` array **directly after `account`**, before `privacy`. The modal shell, sidebar and header need no changes.

```tsx
{
  id: "appearance",
  label: "Appearance",
  icon: (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="9" />
      <path d="M12 3a9 9 0 0 0 0 18 4.5 4.5 0 0 0 0-9 4.5 4.5 0 0 1 0-9z" />
    </svg>
  ),
  component: AppearanceTab,
}
```

### Layout
New file `src/features/settings/tabs/AppearanceTab.tsx`. The tab body inherits the modal's `px-8 pb-7` padding, so the content width is 546px (820 modal − 210 sidebar − 64 padding).

- Section label: `Theme` — 11px, weight 600, uppercase, `tracking-[0.07em]`, `text-text-muted`, 10px bottom margin.
- Grid: `grid-cols-2 gap-3`. Five cards, so the last one sits alone in the left column.
- Card: 261×132, `rounded-lg`, `border border-border-divider`, `bg-bg-light`, `p-2.5`, `cursor-pointer`, `transition-colors`, `hover:bg-bg-lighter`.
- Selected card: `border-accent` + `shadow-ring` (`0 0 0 2px var(--color-accent-soft)`) and a 16px accent check-circle in the top-right of the swatch.
- Card contents, top to bottom:
  1. **Swatch** — 100% × 76px, `rounded-md`, `overflow-hidden`, laid out as a miniature of the app: a 10px-wide chrome column, a 26px sidebar column, then a content area holding two 4px-tall bars (one at 60% width in `text-secondary`, one at 40% in `text-muted`) and one 12px accent pill. Build it from the theme's own tokens so each swatch previews its palette. Do not screenshot the app for these.
  2. **Row** — theme name (13px, weight 500, `text-text-primary`) on the left; a mode chip on the right (`Dark` / `Light`, 9.5px mono, uppercase, `tracking-[0.1em]`, `text-text-muted`, `bg-surface-hover`, `rounded-sm`, `px-1.5 py-0.5`).

### Interaction
- Click anywhere on a card → `setTheme(id)`, applied immediately, then `saveSettings()`.
- Keyboard: cards are `<button>`s with `role="radio"` inside a `role="radiogroup"`; Arrow keys move selection, Space/Enter commits. Focus ring = `shadow-ring`.
- No confirmation, no restart, no toast. The whole modal re-paints under the new tokens as soon as the attribute flips — this is the feedback.
- Transition: put `transition-colors duration-150` on the app root so the swap eases rather than snapping. Do **not** transition `box-shadow` on the server bar (there's a note in `globals.css` about the repaint cost).

## State management

Add to `src/stores/uiStore.ts`:

```ts
export type ThemeId = "graphite" | "graphite-light" | "console" | "console-light" | "console-split";
// state:  theme: ThemeId  (default "graphite")
// action: setTheme(t: ThemeId) → set({ theme: t }); document.documentElement.dataset.theme = t;
```

Set the attribute in three places:
1. `setTheme` (live switching).
2. `loadSettings()` after hydration, so first paint is correct.
3. `electron-client/index.html` — inline `<script>` reading a mirrored `localStorage` copy before React mounts, to avoid a flash of the default dark theme on cold start when the persisted theme is light. Write that mirror inside `setTheme`.

Persistence follows the existing pattern exactly:
- `native/src/config.rs` → add `theme: String` to `AppSettings` with serde default `"graphite"`.
- `saveSettings.ts` → add `theme: ui.theme` to the `invoke("save_settings", {...})` payload. Field names are snake_case-matched by serde; a mis-named key is silently dropped.
- `loadSettings.ts` → add `theme: string` to `LoadedConfigShape.settings` and hydrate through a whitelist check (`["graphite","graphite-light","console","console-light","console-split"].includes(...)`), falling back to the store default — same defensive style as the `stream_resolution` validation above it.

No new IPC command is needed.

## Change: media audio attachment player

`src/features/chat/AttachmentList.tsx` (audio row, ~L447–L706) already implements volume and download; the earlier mocks omitted them. They are now in the design and the spec below matches the shipped behavior — treat this as **token-only work on existing controls**, plus the layout confirmation below.

Card: 390–394px wide, `rounded-lg` (theme radius), `bg-bg-light`, raised elevation, 10–11px padding, 10–11px gap.

- **Left:** 34–36px square waveform tile, `bg-accent-soft`, theme radius, containing three 2.5px accent bars animating `waveBar` at 0 / .18s / .36s delays.
- **Top row:** filename (12.5px / weight 500 — 11.5px mono in `console*`, `text-text-primary`, ellipsised, `flex:1`) then a **20px download button** (13px stroke icon, `text-text-muted`, `hover:text-text-primary` + `hover:bg-surface-hover`, theme radius, `title="Download"`). Wired to the existing `onDownload` handler.
- **Meta row:** `3:58 · 3.6 MB` — 9.5–10.5px mono, `text-text-muted`.
- **Control row** (`display:flex; align-items:center; gap:9px`):
  1. Play/pause — 22–24px, `bg-accent`, `on-accent` glyph; circle in `graphite*`, 3px square in `console*`.
  2. Scrub track — `flex:1`, 2–3px tall, `bg-surface-active`; filled portion `bg-accent`.
  3. Time — 9.5–10px mono, `text-text-muted` (`0:00 / 3:57`).
  4. **Speaker toggle** — 15px, 13px stroke icon, `text-text-muted`. Muted or volume 0 swaps to the muted glyph (existing behavior).
  5. **Volume track** — 46px wide, `flex:none`, same height/radius as the scrub track, `bg-surface-active`; fill `bg-accent` at the current level; 8px round accent knob centred on the fill edge.

Existing behaviors to preserve: click-drag anywhere on the volume track sets level; scroll wheel over the track adjusts in steps (listener attached imperatively with `passive:false`); level and mute live in `uiStore.mediaAudioVolume` / `mediaAudioMuted` via `audioSetVolume`, shared by every audio row and persisted through `saveSettings`.

## Interactions & behavior (unchanged, re-tokenised)
- Hover on message rows: `hover:bg-white/[0.015]` — in light themes use `hover:bg-black/[0.02]`.
- Hover on channel / member rows: `bg-surface-hover`; active: `bg-surface-active`.
- Hover transitions stay at the `--default-transition-duration: 100ms` default; transform/layout transitions opt into 150ms.
- Existing keyframes (`fadeUp`, `dropIn`, `cardIn`, `pulse-dot`, `waveBar`, `breathe`, `toastIn`, `pickerIn`, `tooltipIn`, `dropPulse`) are unchanged, but `dropPulse`'s hard-coded `rgba(69,150,255,…)` must become `color-mix()` or per-theme custom properties, or drag-and-drop glows blue in the mint themes.
- Focus states: every interactive element gets `--shadow-ring`. In light themes bump the ring alpha (`accent-soft` at .11 is too faint to read as focus) to `rgba(accent,.22)`.

## Assets
None new. Icons are the app's existing Feather-style inline SVGs — the mock's mic / headphones / gear / leave / screen-share paths were lifted from `features/channels/UserPanel.tsx`, window controls from `layouts/Titlebar.tsx`, and download / speaker / home / emoji / send / plus are Feather equivalents drawn to the same 24×24, `strokeWidth="2"`, round-cap grid. One new font package: `@fontsource/ibm-plex-mono`.

## Suggested implementation order
1. Indirect the `@theme` tokens through `--t-*` variables; add the `graphite` block with today's values *reworked to the table above*. Ship this alone and confirm the app looks identical to the mock's `1a`.
2. Add `uiStore.theme` + `setTheme` + the `data-theme` attribute plumbing, with no UI — flip it from devtools to test.
3. Add the four remaining theme blocks. Audit contrast per the rule above.
4. Add `AppearanceTab` and register it in `TABS`.
5. Persist: `config.rs`, `saveSettings`, `loadSettings`, plus the pre-mount `index.html` script.
6. Audio player: download button + volume cluster tokenised as specced.
7. Sweep for hard-coded colors outside the token set — `dropPulse`, `stringToColor`, `custom-slider` thumb borders, `MembersList` status-dot borders, and any `bg-white/[…]` / `text-white` literals.

## Files in this bundle
- `Chat Polish.dc.html` — the design reference. Five themes, each a full main-chat screen with id badges (`1a` = `graphite`, `1c` = `console`, `1d` = `graphite-light`, `2a` = `console-light`, `2b` = `console-split`). Turn 2 (top) holds the light console work; turn 1 (below) holds the original three directions plus `1b` Warm Editorial, which was **not** selected for implementation — ignore it.

## Repo context
Built against `sunkhan/decibell`, branch `main`, from `electron-client/src`. Files read while designing: `styles/globals.css`, `layouts/MainLayout.tsx`, `layouts/Titlebar.tsx`, `features/chat/MessageBubble.tsx`, `features/chat/AttachmentList.tsx`, `features/friends/MembersList.tsx`, `features/channels/UserPanel.tsx`, `features/settings/SettingsModal.tsx`, `features/settings/{loadSettings,saveSettings}.ts`, `features/settings/tabs/PrivacyTab.tsx`. The `tauri-client` tree mirrors `electron-client` — if it is still maintained, every change here applies there too.
