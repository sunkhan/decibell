# Global hotkeys: mute, deafen, push-to-talk, push-to-mute, leave, answer / decline

Status: design 2026-10-10. Client-only (Electron main + native addon + renderer);
no proto or server work.

## Goals

Keybinds that work while Decibell is not focused (in a game, in another app):

| action | kind | effect |
|---|---|---|
| `toggle_mute` | press | same as the mic button (unmute while deafened also undeafens) |
| `toggle_deafen` | press | same as the headphones button |
| `push_to_talk` | hold | in Push-to-talk input mode, the mic transmits only while held |
| `push_to_mute` | hold | the mic stops transmitting while held (any mode) |
| `leave_voice` | press | leave the voice channel (stops a stream first) or hang up the DM call |
| `answer_call` | press | accept the ringing incoming DM call |
| `decline_call` | press | decline the ringing incoming DM call |

Owner decisions (2026-10-10): all seven actions; macOS gets the press actions
through Electron's `globalShortcut` only (no Input Monitoring prompt), with hold
actions working while the window is focused. **Bound keys only** (revised the
same day): no backend may receive a stream of everything typed. Global input
hooks, raw input and XInput2 raw events "give keylogger vibes", so the first
cut's Raw Input / XInput2 listeners were replaced (see "Bound keys only").

## Why not Electron `globalShortcut` everywhere

- Electron 33 has no Wayland backend for it (no `GlobalShortcutsPortal` in the
  binary), and the client runs native Wayland (`ozone-platform-hint=auto`).
- It fires on press only, never on release, so push-to-talk is impossible with it.

So the global listener lives in the native addon, one backend per platform,
and macOS is the only place that uses `globalShortcut`.

## Key model

Bindings are recorded in the renderer from `KeyboardEvent.code` (physical keys,
layout-independent) and `MouseEvent.button`:

- keys: DOM codes (`KeyM`, `F13`, `Backquote`, `NumpadAdd`, …);
- modifiers: generic `Control` / `Shift` / `Alt` / `Meta` when the combo has a
  non-modifier key; sided (`ControlRight`) when the combo is modifier-only
  (a lone Right Ctrl as push-to-talk);
- mouse: `Mouse3` (middle), `Mouse4` (back), `Mouse5` (forward). Left/right
  clicks can't be bound.

`native/src/hotkeys/keys.rs` maps DOM code → evdev code (X11 keycode − 8) and
Windows set-1 scancode (`0xe0xx` = extended), generated from Chromium's
`ui/events/keycodes/dom/dom_code_data.inc`, plus the XKB keysym name the
portal's `preferred_trigger` wants.

```
binding = { id: string, action: Action, keys: string[] }
```

`id` is stable for a binding's lifetime; re-recording a binding's keys mints a
new id (the portal keeps a user's assignment per id, so a changed preferred
trigger needs a fresh id to take effect).

### Matching (X11 / Windows / focused fallback)

`hotkeys/matcher.rs` (mirrored in `features/hotkeys/keys.ts`) is fed
transitions of the *watched* keys only: the keys the bindings name (a generic
modifier → both sides) plus the eight modifier keys when a press binding
needs its modifiers to match exactly (`matcher::watched_keys`).

The listener tracks the set of held physical keys. A generic modifier in a
binding matches either side.

- Hold actions: active while every key of the binding is held (extra keys
  allowed, so Shift+Mouse4 still talks when PTT is Mouse4).
- Press actions: fire on the key-down that completes the binding, only if the
  held modifiers are exactly the binding's modifiers (Ctrl+Shift+M does not
  fire a Ctrl+M binding). Auto-repeat never re-fires.

## Bound keys only

| backend | what Decibell learns | bound key swallowed? |
|---|---|---|
| `portal` | `Activated` / `Deactivated` for its own shortcut ids | yes (the desktop grabs it) |
| `x11` / `xwayland` | a grabbed press of a bound combo; for a held key combo, its release (see below) | yes (the press) |
| `windows` | the state of the watched keys, polled | no |
| `electron` (macOS) | its own accelerators firing | yes |
| focused fallback | keys typed into Decibell's own window | no |

Discord, for comparison, is closed source but every observable trait says it
listens to all input: macOS push-to-talk needs Input Monitoring /
Accessibility, the common Windows failure is an elevated game (a global hook
blocked by UIPI), and on Wayland it works only while an XWayland window is
focused.

## Backends (`native/src/hotkeys/`)

| platform | backend | press | hold | mouse |
|---|---|---|---|---|
| Linux, Wayland with the GlobalShortcuts portal | `portal` | ✓ | ✓ (`Deactivated`) | ✗ |
| Linux, Wayland without the portal, XWayland present | `xwayland` | while an X11 app is focused (most games) | same | ✓ |
| Linux, X11 session | `x11` (passive grabs on the bound combos) | ✓ | ✓ | ✓ |
| Windows | `windows` (`GetAsyncKeyState` on the watched keys) | ✓ | ✓ | ✓ |
| macOS | `electron` (main process `globalShortcut`) | ✓ | focused only | ✗ |

Any backend that can't see keys while Decibell itself is focused (`xwayland`,
hold actions on macOS) turns on the renderer's **focused fallback**: window
`keydown` / `keyup` / `mousedown` / `mouseup` run the same matcher and inject
the result. Window blur releases every held fallback binding.

### Wayland: `org.freedesktop.portal.GlobalShortcuts`

zbus (already a Linux dependency), own connection, own thread.

1. `org.freedesktop.host.portal.Registry.Register("decibell", {})`. It must be
   the first call on the connection. Accepted only when `decibell.desktop` is
   installed, which is true for every package. Failure is ignored, because the
   portal falls back to the systemd-scope app id.
2. `CreateSession` → `BindShortcuts(session, [(id, {description,
   preferred_trigger})], "", {})`. KDE shows its confirmation dialog for ids it
   hasn't seen; known ids bind silently. The result's `trigger_description`
   per id is the truth and is shown in the Keybinds tab next to the in-app
   combo.
3. `Activated` / `Deactivated` → press / release for that binding id.
   `ShortcutsChanged` updates the shown triggers.
4. Bindings changed → close the session, open a new one, bind again.
5. Portal v2: `ConfigureShortcuts` backs the "Change in system settings"
   button.

No bindings means no session, so a user who never opens Keybinds never sees a
dialog.

**Mouse buttons on Wayland.** The shortcuts spec behind `preferred_trigger`
describes modifiers + one keyboard key, so the portal can't bind a mouse
button, and nothing outside the compositor may watch the mouse:
`/dev/input/event*` is `root:input 0660` and logind's `uaccess` covers
joysticks only, by design. Reading evdev would need the `input` group, which
exposes every keyboard too — ruled out (bound keys only). So a side button
becomes a key in the desktop first: recording a mouse button on the
`portal` backend opens a guide in that row instead of saving it. KDE (Plasma
6.1+, verified on 6.7.5): System Settings → Mouse → Extra Mouse Buttons →
Add Binding (KWin's `buttonsrebind`), with an "Open mouse settings" button
(`systemsettings kcm_mouse`); suggest a typeable Meta combo (the dialog
records keys, most keyboards have no F13, and games rarely bind Super).
GNOME: no built-in remap (Piper / input-remapper). Others: their input
settings, or the command line. Decibell never writes desktop config.

### X11: passive grabs on the bound combos

X11 has no passive per-key subscription (XInput2 raw events and XRecord both
deliver everything), so x11rb (pure Rust, no libX11) grabs exactly the bound
combos on the root window:

- each binding grabs its main key (the non-modifier, or the last key of a
  modifier-only combo; keycode = evdev + 8) or button (2/8/9 = Mouse3/4/5)
  with the modifiers the rest of the combo requires, in every Caps Lock /
  Num Lock variant. A hold with no modifiers grabs under `AnyModifier`, so
  Shift+V still talks when push-to-talk is V. Alt / Super / Num Lock masks
  come from the server's modifier mapping;
- grabs are **synchronous**: on a grabbed press the server freezes input, we
  ungrab at once, and anything typed in that instant goes to the focused app
  afterwards, never to us;
- the grab already matched the exact modifiers, so a **press action fires
  straight from the grabbed event** — no state is read;
- a **hold** needs its release, which X reports only to whoever has the
  key, so it is polled every 10 ms while down, asking as little as the
  binding allows: a mouse button or a lone modifier key → `XIQueryPointer`
  (buttons + modifier state only); an ordinary key → `XQueryKeymap`, which
  is X's only key-state question and lists every key down at that moment
  (only the bound key's bit is read). The keymap is asked only while such
  a hold is down; the e2e test counts the reads and fails otherwise. The
  Keybinds tab says exactly this;
- a grab another client holds fails with BadAccess → that binding id goes
  into `Status::failed` ("Another app already uses this shortcut").

The thread sleeps in `poll()` on the X socket and a wake-up pipe (no idle
timer), regrabs when the bindings change and exits — dropping every grab —
when the last binding goes. The trade-off of grabbing: the bound press
doesn't also reach the focused app, so the Keybinds tab warns when a
typing key (e.g. bare V) is bound on a grabbing backend.

### Windows: polling the watched keys

`GetAsyncKeyState` answers for one key, so a thread polls only the watched
keys every 10 ms and feeds transitions to the matcher. Nothing is
registered or hooked, nothing is swallowed, mouse buttons and modifier-only
combos work. Layout-independent keys use a fixed virtual-key table
(`keys::win_fixed_vk`); character keys go through
`MapVirtualKey(scancode, VSC_TO_VK_EX)` under the active layout whenever the
bindings change, so they stay the recorded physical keys. Costs: up to one
poll of latency, a sub-10 ms tap can be missed, and both Enters share one
virtual key. The thread exists only while a binding does. An elevated
(admin) game may hide its keys from a non-elevated Decibell.

### macOS

`electron/main/hotkeys.ts` registers press-action bindings as accelerators
(`Control+Shift+M`; `Meta` → `Command`) and injects into native on fire.

### Command line (any platform)

`decibell --hotkey=<action>` (holds: `--hotkey=push_to_talk:down` /
`:up`) goes through the existing single-instance `second-instance` handler.
That handler skips focusing the window for these. If no instance is running,
the launch quits immediately. This is the escape hatch for Sway / Hyprland
without a portal, or GNOME < 48: bind the command in the compositor.

## Dispatch

Every source (backend, focused fallback, macOS accelerators, CLI) ends in
`hotkeys::dispatch(source, action, pressed)` in native:

- hold actions: a per-action set of held sources. `push_to_talk` /
  `push_to_mute` drive the voice gate directly in native, so a press never
  waits on the renderer.
- every transition is emitted as `hotkey_action { action, pressed }`. The
  renderer runs press actions through `features/voice/voiceActions.ts` (the
  one copy of the mute / deafen / leave logic, replacing the three in
  UserPanel, VoicePanel and CallStage) and `callActions.acceptCall` /
  `declineCall` when a call is ringing.
- `hotkeys_set_paused(true)` while the Keybinds tab is recording, so
  recording Ctrl+M doesn't also toggle mute.

## Voice gate (`native/src/media/voice_gate.rs`)

Process-wide atomics read once per 20 ms frame by the capture loop
(`pipeline.rs`), so the state survives voice sessions and DM calls alike:

- `input_mode`: voice activity | push-to-talk (`set_input_mode`);
- `ptt_held`, `ptt_release_delay_ms` (default 100, 0–1000): the gate stays
  open for the delay after release so the last syllable isn't clipped;
- `ptm_held`.

In the capture loop: muted, PTM held, or PTT mode not held means the gate is
forced closed. PTT mode held means the gate is forced open, and the VAD
threshold is ignored (Discord's behaviour). Otherwise the VAD runs as today.
A forced close is a closed gate, not a mute: no `FLAG_MUTED`, the 60 ms fade
tail still runs, and peers see you as not speaking rather than muted. In PTT
mode the pre-roll frame (audio from *before* the key went down) is not sent;
the first frame fades in instead. The local speaking ring follows the gate in
PTT mode.

## Settings

`AppSettings` (renderer stays the source of truth, `saveSettings` /
`loadSettings`):

- `hotkeys: [{ id, action, keys }]` (default empty, no bindings out of the box);
- `input_mode: "voice_activity" | "push_to_talk"`;
- `ptt_release_delay_ms`.

UI:

- **Settings → Keybinds** (new tab): the backend status line (what works
  here, and how), a row per binding (action select, recorder, portal trigger
  where it differs, delete), "Add keybind", "Change in system settings" on
  portal v2, and the `--hotkey` command line for desktops with no global
  support.
- **Settings → Audio**: Input mode (Voice activity / Push to talk). PTT shows
  the release-delay slider and hides the sensitivity slider. A warning
  appears when PTT is on but no push-to-talk key is bound.

## Testing

- Native unit tests: key table round-trips, matcher (supersets, exact
  modifiers, repeats, sided modifiers), voice-gate timing.
- `pipeline` gate behaviour through the existing voice sim tests.
- Live: KDE Wayland portal (dialog, Activated / Deactivated, rebinding,
  ShortcutsChanged); X11 and Windows by hand; macOS press actions.
- X11 end to end: `hotkeys::tests::x11_listener_end_to_end` (ignored) drives
  the real listener against a disposable X server with XTEST:
  `Xvfb :97 & DECIBELL_X11_TEST_DISPLAY=:97 cargo test --lib x11_listener -- --ignored`.
- Windows poller: Linux-side type-check crate + Windows Native Check CI.
