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
actions working while the window is focused.

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

The listener tracks the set of held physical keys. A generic modifier in a
binding matches either side.

- Hold actions: active while every key of the binding is held (extra keys
  allowed, so Shift+Mouse4 still talks when PTT is Mouse4).
- Press actions: fire on the key-down that completes the binding, only if the
  held modifiers are exactly the binding's modifiers (Ctrl+Shift+M does not
  fire a Ctrl+M binding). Auto-repeat never re-fires.

Passive listening: keys are observed, never swallowed, so the focused game
still gets them (Discord's behaviour).

## Backends (`native/src/hotkeys/`)

| platform | backend | press | hold | mouse |
|---|---|---|---|---|
| Linux, Wayland with the GlobalShortcuts portal | `portal` | ✓ | ✓ (`Deactivated`) | ✗ |
| Linux, Wayland without the portal, XWayland present | `xwayland` | while an X11 app is focused (most games) | same | ✓ |
| Linux, X11 session | `x11` (XInput2 raw events) | ✓ | ✓ | ✓ |
| Windows | `windows` (Raw Input) | ✓ | ✓ | ✓ |
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
dialog. Mouse bindings are skipped on this backend; the tab says so.

### X11: XInput2 raw events

x11rb (pure Rust, no libX11 link), `XISelectEvents(root, XIAllMasterDevices,
RawKeyPress|RawKeyRelease|RawButtonPress|RawButtonRelease)`. Raw events reach
every client regardless of focus or grabs. Keycode − 8 = evdev code. Buttons
2/8/9 = Mouse3/4/5.

### Windows: Raw Input

A thread owns a message-only window and
`RegisterRawInputDevices(keyboard [+ mouse], RIDEV_INPUTSINK)`. The scancode is
`MakeCode | (E0 ? 0xe000 : 0)` with the known fixups: VKey 0xFF (fake shift)
dropped; the `E1` half of Pause becomes `0x0045`; `VK_NUMLOCK` becomes
`0xe045`; MakeCode 0 falls back to `MapVirtualKey(VK, VK_TO_VSC_EX)`. Mouse is
registered only while a binding uses a mouse button, because every mouse move
is a WM_INPUT. Known limit, shared with Discord: no input while an elevated
(admin) window has focus.

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
- Windows Raw Input: Linux-side type-check crate + Windows Native Check CI.
