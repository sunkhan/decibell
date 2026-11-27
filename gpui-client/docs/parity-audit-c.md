# Parity audit C: voice, streams, app and settings (FEATURES §7, §8, §9)

2026-11-27. Each row of `docs/FEATURES.md` §7–§9 was split into its rules. Each rule was read in the Electron
client and then looked for in the native client. The self-tests that are the rows' evidence were run one at a
time on the frozen build (`~/.cache/decibell-dev/parity-root`, copied 12:06 from the branch head `51be7d7`)
against the local stack. The checklist is `gpui-client/PARITY.md` (rows 7.x, 8.x, 9.x).

Path shorthand:
- `R/` = `electron-client/src/`
- `M/` = `electron-client/electron/main/`
- `core/` = `crates/decibell-core/src/`
- `N/` = `gpui-client/crates/`
- `MD/` = `N/decibell-model/src/`
- `V/` = `N/decibell-voice/src/`
- `S/` = `N/decibell-settings/src/`

Sizes: S = under an hour, M = hours, L = a day or more.

**Totals.**

| Section | Rows | done | needs owner | partial |
|---|---|---|---|---|
| §7 Voice channels | 11 | 0 | 0 | 11 |
| §8 Screen sharing | 8 | 0 | 1 (8.8) | 7 |
| §9 App, settings, shell | 14 | 0 | 0 | 14 |

Nearly everything is built. Most rows are `partial` for one of three reasons:
- a real difference from Electron (§1 below);
- a rule nothing runs although a self-test could (§2);
- both.

No row reaches `done`: each has at least one rule that is either different from Electron or testable here and untested.

## 1. Product gaps, most important first

1. **Changing the avatar does nothing (9.1). M.**
   - Electron: `R/features/settings/tabs/AccountTab.tsx:91-104,143-148` (click, "Change picture" or "Add a picture" → file chooser → `AvatarCropperModal` → `upload_avatar`).
   - Native: `S/tabs/account.rs:104-107` emits `PictureEvent::ChangeAvatar` into `PictureRequests` (`S/hooks.rs`), and nothing on the branch subscribes to it (no other crate names `PictureEvent` or `PictureRequests`).
   - The working editor host is on the unmerged `wip/integrate-d` (`N/decibell-app/src/avatar.rs` plus an `avatar-editor` self-test).
   - To close it: land that work.
   - Also missing on the same card: drop a picture onto it (`R/components/pictureEditor/PictureButton.tsx:10-53`); the just-saved picture shown until the store fetches it (`AccountTab.tsx:65-76`); the picture as a focusable button (`account.rs:129-139` is a `div` with no focus). S each.

2. **Auto codec pins H.264, so the software encoder is never reached (8.1, 8.2). S.**
   - Native: `MD/actions/voice.rs:875,886-887` sends `initial_codec = enforced_codec = start_codec()`, and `start_codec()` maps Auto to H.264 (`MD/live/voice/go_live.rs:212-214`).
   - Core: `core/api/streaming.rs:538` → `core/media/encoder_backend.rs:553-555` adds the SVT-AV1 candidate only when the codec is AV1 or not locked.
   - Effect: on a machine without a working hardware H.264 encoder, the start fails with "Codec 1 not available on this hardware". Meanwhile the dialog's hint, `V/codecs.rs:102-108` ("the core does the same when not enforced"), promises "Software (SVT-AV1)". This breaks the owner rule "hardware first, software as the last resort".
   - Electron sends the same pinned pair (`R/features/voice/streaming/StreamCapture.ts:176,215-216`) but falls back to WebCodecs on failure (`:228-257`); the native client has no such fallback.
   - To close it: for Auto send `initial_codec 0` (the core's `auto_codec`) and `enforced_codec 0`. Add a self-test with `DECIBELL_ENCODER=software` that presses Go Live and expects the plan to reach SVT-AV1. The error surfaces before the portal.

3. **`panic = "abort"` in the release profile turns every recoverable media panic into a crash (9.14). M, owner decision.**
   - The profile is in `gpui-client/Cargo.toml` `[profile.release]`.
   - The core catches panics at `core/media/video_recv_host.rs:164`, `encoder_probe.rs:461`, `pipeline.rs:462`, `media/mod.rs:371`, `encoder_thread.rs:167` and `encoder_thread_linux.rs:215`, in the Windows capture files, and in `N/decibell-media/src/source.rs:52,70`. With abort, none of these catches work.
   - Electron's addon unwinds, so a decoder or encoder panic it survives kills the whole native app.
   - To close it: decide. Either unwind in the ship build (measure the size), or keep abort and record the change, with the crash reporter as the safety net.

4. **A watched stream keeps decoding while the window is minimised or hidden (8.4). M.**
   - Electron: `R/features/voice/StreamVideoPlayer.tsx:93-95,516-528` stops feeding the decoder while `document.hidden` and asks for a keyframe on return.
   - Native: the hub decodes while any `Viewer` exists (`gpui-client/docs/video-design.md:76`), and nothing in `decibell-voice`, `decibell-video` or `decibell-app/src/video.rs` watches the window's visibility.
   - This is a resource rule and is not recorded as a quirk.
   - To close it: on minimise or occlusion, release the frame-sink interest (or gate the hub); on return, request a keyframe. Add a self-test that minimises through KWin, checks `decoded` stays flat and decoder threads go to 0, restores and expects a picture.

5. **A click on another Settings tab with a custom speed typed in Network doesn't open that tab (9.7, 9.10). S–M.**
   - `settings-unsaved-shake` failed 5 of 5 runs. Four times it said "a press on another tab with a typed value didn't open it"; once it said "still drawing after 15 s" after the shake.
   - Electron (`R/features/settings/tabs/NetworkTab.tsx`) has no guard: the box's `onBlur` applies the value and the tab opens.
   - Native: the blur is meant to apply the value before the release opens the tab (`S/tabs/network.rs:194-201,484-494`; the test's comment explains the press, frame, release order at `S/selftests.rs:1453-1466`). The tab never opens.
   - It is either the window's leave guard seeing the draft at the release, or the blur arriving late. Read the guard path in `S/window.rs` with the test.

6. **The voice view draws frames at rest, intermittently (7.3). M to find.**
   - `voice-speaking-idle` failed 3 of 5 runs (2 of 3 alone under the wrapper's lock).
   - Each failing try was unfocused: 4–10 frames drawn in 10 s in Lounge with a quiet, muted peer, and 0–2 latency changes. The test allows one frame per latency change.
   - Passing tries drew 0–1.
   - `DECIBELL_VOICE_TRACE=1 DECIBELL_LOG=info` was set on a run, but that run passed and logged no renders. To find it: repeat until it fails with the trace on.

7. **The stream-audio gain likely stays at 0 after you preview your own stream (8.4). S.**
   - `V/focus.rs:129-136` mutes the global stream gain while our own stream is focused. It is restored only when the focus moves inside the same `FocusView`.
   - `V/pane.rs:265-266` drops the `FocusView` when the watch list empties, and `FocusView` has no `on_release` or `Drop`.
   - So: preview your own stream, then Stop watching or Stop sharing. Every stream watched later is silent while the slider shows 100 %, because a new `FocusView` starts with `muted_for_preview = false` and never sets the gain.
   - Electron restores it in its unmount cleanup (`R/features/voice/StreamViewPanel.tsx:235-248`).
   - To close it: restore the gain on release. Add a gpui unit test.

8. **F8 / Shift+F8 change the theme in every build, ship included (9.3). S.**
   - `N/decibell-app/src/boot.rs:241,333-348` (`bind_theme_keys`) is not behind `cfg(feature = "dev")`.
   - The theme isn't saved, so Settings → Appearance and the next start disagree with the screen.
   - F8 can't be recorded as a keybind, because GPUI dispatches the binding first.
   - To close it: gate it under `dev`. The `themes` self-test uses it, and that stays fine.

9. **There is no keyboard access in the voice view or the user panel's voice buttons (7.3). M.**
   - Electron's stage tiles are tab stops, and Enter / Space opens the profile (`R/features/voice/stage/StageTiles.tsx:78-84,260-266`).
   - Its dock and user-panel controls are buttons.
   - Native: there is no `track_focus` anywhere in `decibell-voice`; `N/decibell-shell/src/user_panel.rs:112-153` builds the panel's buttons as clickable `div`s.
   - This goes against `CONVENTIONS.md:95-96` ("every control is a tab stop").

10. **Keybind labels ignore the keyboard layout (7.11, 9.6). M.**
    - Electron: `R/features/hotkeys/keys.ts:172-209` labels keys through `navigator.keyboard.getLayoutMap()`.
    - Native: `MD/live/hotkeys.rs:332-414` uses US names, so the German key labelled Z shows "Y". Recording is correct (physical codes); only the label is wrong.
    - To close it: label through the layout (xkb on Linux, `ToUnicodeEx` / `MapVirtualKey` on Windows). Or record it as a quirk.

11. **Moving a member by drag starts where Electron refuses (7.2). S.**
    - Electron: `canMoveMember` (`R/features/voice/memberDrag.ts:47-58`) refuses ourselves, the owner and anyone `canManageMember` refuses, so a refused drag never starts.
    - Native: `N/decibell-shell/src/channels.rs:394,533-535` checks only `movable && uid != me && uid > 0`. The owner and members above us can be dragged, and the server refuses with a toast.
    - Also different:
      - Electron's drop zone is the channel row plus its member list, and every eligible channel pulses while a drag is in flight (`R/features/channels/ServerChannelsSidebar.tsx:1023,1031`). Native accepts drops on the row only (`channels.rs:446-460`), with no pulse. S.
      - The failure toast reads "Couldn't do that" (`MD/actions/voice.rs:502-504`) where Electron says "Couldn't move member" (`memberDrag.ts:114-116`) or "Voice moderation failed". S.

12. **The mic test is dead during a DM call (7.5, 9.5). S.**
    - `S/tabs/audio.rs:140,176` treat "connected" as `session.channel.is_some()`, which is false in a call.
    - In a call the button shows, the core refuses the test, the error is only logged, and the button reads "Stop test" over a dead meter.
    - Electron's meter follows the voice session in a call too (`R/features/settings/tabs/AudioTab.tsx:210-226`).
    - To close it: use `media_session_active()`.

13. **The connection-stats and stream-audio popovers outlive the session (7.10). S.**
    - `V/state.rs:66-80`: the popover closes only on a second click, a press outside, Esc or blur (`V/menus.rs:360-371,624,640`).
    - After a moderator disconnect or a hang-up it stays open, with nothing behind it.
    - Electron renders it inside the in-session block (`R/features/channels/UserPanel.tsx`).
    - To close it: close it when `media_session_active()` turns false.

14. **The Go Live dialog's leftovers (8.1). S each.**
    - The dialog stays open after the session ends, and Go Live then silently does nothing (`MD/actions/voice.rs:857-859`).
    - `own_stream.error` and `source_id` survive closing the dialog (`MD/live/voice/streams.rs:428-439`).
    - On a software-only machine, a greyed preset that is already chosen (the default Smooth) stays chosen, and Advanced can go past the encoder's ceiling (`V/go_live.rs:961-992`).
    - Windows layout differs and isn't recorded. Electron uses a 1040 px dialog with a row of four source cards and three window columns, a footer with the picked source's still and name, and Go Live disabled before a pick (`R/features/voice/CaptureSourcePicker.tsx`, `goLive/SourceGrid.tsx`). Native uses the Linux layout and errors on press (`MD/actions/voice.rs:866-872`). M + S.

15. **The full view doesn't grow back out of the mini-player (8.5). S.**
    - Electron: `R/features/voice/StreamViewPanel.tsx:118-158`.
    - Native: `V/mini.rs:311` records `mini_rect`, but `V/state.rs:28,88` is never read.

16. **The updater's restart (9.11, 9.9). S each.**
    - Restart to update calls `cx.quit()` (`N/decibell-app/src/updater.rs:177-186`), which skips the upload quit guard. Electron's `before-quit` asks (`M/index.ts:838-840`).
    - `relaunch_if_requested` passes `trial_args()`, the original argv (`updater.rs:225-232`), so an old `decibell://invite/…` reopens after a restart.
    - A trial start that presents no frame within 60 s (for example, a locked screen) is recorded as failed, and that version is skipped for good (`N/decibell-release/src/apply.rs:130-145`).
    - The failed-trial notice is written to About at start (`updater.rs:76-84`), then replaced by the first check 5 s later (`:93-100`). It is also marked shown at once, so nobody sees it.

17. **Small behaviour differences. S each.**
    - A second click on the "+N more" tile reopens the list: the popover's outside-press closes it, then the tile's click toggles it open (`N/decibell-ui/src/popover.rs:315`, `V/tiles.rs:732-736`). Electron ignores presses on the tile (`StageTiles.tsx:629`). (7.3)
    - An empty keybind row is saved and comes back after a restart (`S/tabs/keybinds.rs:410-416` pushes it). Electron filters `keys.length > 0` on save (`R/features/settings/saveSettings.ts:87-89`). (7.11)
    - Network's "system Downloads" default reads only the `XDG_DOWNLOAD_DIR` environment variable (`S/tabs/network.rs:136-143`), while downloads go to `os_downloads_dir()`, which reads `user-dirs.dirs` (`N/decibell-chat-content/src/downloads.rs:23-40`). The tab names one folder and the file lands in another. (9.7)
    - The join dialog's Cancel, Accept and × are click-only `div`s (`N/decibell-servers/src/join.rs`); Electron's are buttons. (9.12)
    - An unknown command-line argument makes the app exit 2 (`N/decibell-app/src/args.rs:131`); Chromium ignores unknown switches. (9.12)
    - Every theme card is a Tab stop (`S/tabs/appearance.rs:67`); Electron uses a roving tabindex (`R/features/settings/tabs/AppearanceTab.tsx:316-332`). (9.3)
    - Crash reports lack the renderer's `connected_servers` tag (`R/layouts/AppLayout.tsx:64-80`) and the device / app / GPU contexts. The native client sends `contexts.os` only (`N/decibell-release/src/crash/mod.rs:245-252,322-353`). (9.14)
    - Text that Electron marks `select-text` can't be selected: the device address (`S/tabs/devices.rs:145`), the E2EE fingerprint (`S/tabs/privacy.rs:259-270`), and the keybind command lines (`S/tabs/keybinds.rs:906-914`). (9.2, 9.4, 9.6)
    - Missing tooltips:
      - sidebar "Muted by you" (`N/decibell-shell/src/channels.rs:294`);
      - the tile's mic / deafen / muted-by-you glyphs (`V/tiles.rs:183`);
      - "Stream locked to X", "Has audio" and "N watching" (`V/tiles.rs:555-568`, `V/scrim.rs:181`). (7.2, 7.3, 8.4)
    - A thumbnail fetch that came back empty is not retried on a later visit to the voice view (`MD/actions/voice.rs:138,725-779`). Electron's set is per mount (`R/features/voice/useStreamThumbnails.ts`). (8.6)
    - The Devices tab shows the previous list, or error, until the reload lands (`MD/live/devices.rs:143-152`); Electron shows "Loading…". (9.2)
    - The threshold badge rounds −50.5 to −51 (`S/tabs/audio.rs:458`); Electron shows −50. (9.5)
    - The "Open mouse settings" helper never reaps `systemsettings` (`S/tabs/keybinds.rs:452-459`). (9.6)
    - On X11, the 800 × 600 size hint may include the 10 px client-side shadow, giving a visible 780 × 580 (`N/decibell-ui/src/window_frame.rs:20`, `N/decibell-app/src/window.rs:14,42`). Not measured. (9.10)

18. **Differences that aren't recorded.** Record each in `docs/electron-quirks.md` or have the owner decide:
    - The user menu on a voice row is the whole people menu (Profile, Message, friends, nickname, roles, kick, ban, Copy username) with the voice sections joined in (`N/decibell-app/src/registry.rs:95-135`). Electron's `UserContextMenu` has the voice entries only. (7.7)
    - The mic test meters through the user's AEC / NS / AGC. Electron's `getUserMedia` uses Chromium's own processing. (7.5, 9.5)
    - Network's custom box is a SaveBar draft that shakes on leave (recorded only in `CONVENTIONS.md:487-491`), with no arrow / spinner stepping. (9.7)
    - The update chip has no pulse (a comment in `N/decibell-shell/src/user_panel.rs`). (9.11)
    - The updater and the crash sender use Mozilla's roots and no system proxy (`N/decibell-release/src/http.rs:1-9`). Electron uses Chromium's system proxy and OS store. (9.11, 9.14)
    - There are no non-fatal error reports. Electron reports renderer and main-process errors and rejections. (9.14)
    - The file pickers have no type filter (GPUI's portal dialogs take none), including the picture pickers. Electron uses `accept=PICTURE_ACCEPT` (`R/components/pictureEditor/PictureButton.tsx:114`). (HANDOFF §1b)

## 2. Rules nothing runs, though a self-test could

Per row, against the local stack (an app child as perf3 or alice, a `decibell-headless` peer as perf4 / perf5).

**7.1**
- Our own Connect and Disconnect cues: a `CueLog` in `voice-join`.
- The CONNECT_VOICE refusal toast: a unit test on `actions::voice`.
- A DM call hung up by a channel join: in `call-ring` after answering.
- The failed-join path: a unit test, since the core isn't running in unit tests.
- Clicking `dock-leave` and the user panel's Disconnect: every test leaves through the action.

**7.2**
- Probes on member rows: the peer's row shows while the app is out of voice.
- A drag move as alice: perf4 in Lounge, dropped on Squad.

**7.3**
- Switch and Watch too with two streamers.
- Fullscreen Esc and double-click.
- The "+N more" re-click.
- Keyboard focus, once it exists.

**7.4**
- The AEC / NS / AGC / threshold push after the engine starts and after a call connects.
- The channel bitrate at join and its live retune when an admin changes it. Needs a read-back of the engine's settings, through `decibell-headless dump` or a model probe.

**7.5**
- The release delay after a restart: `settings-audio-meter` sets 250 ms and never reads it back.
- The "no push to talk key" hint.
- Dragging the sensitivity bar.
- `hotkey-cli`'s push-to-talk part asserts nothing.

**7.6**
- A device picked during a session moves the engine: two null sinks, without touching the defaults.
- The separate stream output.
- The device menus on the mute / deafen buttons.

**7.7**
- The local-mute row.
- The gain pushed to the engine in a session and replayed on roster changes.

**7.8**
- The voice moderation section, as alice with perf4 in voice: server mute / deafen → badges, Move to…, Disconnect. `user-menu-moderation` never opens it.
- The receiving side: needs a headless `voice-mod` step.
- The no-SPEAK mic.
- Deafen seen by the peer: `voice-mute` checks only the model.

**7.9**
- Locked keys → the Unlock prompt. The unit test asserts only the status.
- The "Unverified member" / "Voice encryption failed" toasts: unit tests delivering `VoiceE2eeState`.
- The header badge states.

**7.10**
- The popover: click `user-panel-latency`; there's no capture either.
- The ping readout's colours.

**7.11**
- Leave, answer and decline by hotkey.

**8.1**
- Auto on a software-only encoder (`DECIBELL_ENCODER=software`, press `go-live-start`).
- STREAM gating.
- The dialog closing when the session ends.
- Advanced writing to settings.
- The fixture source grid (`DECIBELL_GO_LIVE_SOURCES=fixture`).
- `voice-golive-dialog`'s `frames_of` only logs.

**8.3**
- Go Live in "Only these" while a tone plays into the test's null sink: the row appears and a tick is saved.

**8.4**
- Switch and Watch too (perf4 + perf5).
- Back to the grid by click and by Esc.
- A stream ending, or its streamer leaving, while focused and fullscreen: watching cleared, fullscreen left, the `stream_stop` cue.
- The viewer count reaching perf4's `dump voice`.
- Minimised → no decoding, once gap 1.4 is fixed.
- `voice-watch` clicks `focus-stats` and `mini-player` and asserts nothing after either.

**8.5**
- Click returns to the stream.
- ×.
- Drag and flick to a corner, then resize; `layout.prefs()` holds the corner and width.

**8.6**
- Posters on the tiles of unwatched streams. Needs a headless step sending a JPEG through `send_stream_thumbnail` (`core/api/streaming.rs:1163`).

**8.7**
- Carried on a same-server switch, ended without the option. Needs `stream-file` to mark the headless stream live and a settings toggle step.

**9.1**
- Remove picture.
- The Log Out button (it needs a probe), then a second device's list losing this device.
- Rename refusals (a wrong password, the third rename of a day) and the 32-character cap.
- A wrong current password.

**9.2**
- Sign out all others.
- The empty list.
- The "New sign-in" toast's Review devices.

**9.3**
- The time-format control and the stamps re-labelling.
- The sliders' Reset.
- The first frame after a restart in the saved theme.

**9.4**
- The Away select reaching auto-away.
- The unfiltered-GIF flag: `gif-picker` records it and asserts nothing.
- An invite card with previews off.
- The first-run crash banner: the shell fixture marks it shown.
- Passphrase Change, Reset and Forgot.

**9.5**
- Release delay, processing and threshold after a restart.
- The in-voice meter's level.

**9.6**
- The action select and "Switch now".
- The mouse guide.
- That the core listener paused while recording: the test reads the model's flag.

**9.7**
- A cap changed during a running transfer.

**9.8**
- One self-test for the codec switches and their rollback toast, Refresh, and Take stream with me.

**9.9**
- Check now, Restart and the release page. Run them against `release_e2e.py`'s fake GitHub.

**9.10**
- ✕ and backdrop close.
- The rail's hover labels.
- User Settings and Channel Settings below 1024 px. `settings-narrow` opens Server Settings only, though its description names Channel Settings.
- Server Settings' window-level shake.

**9.11**
- The user panel's update chip.
- Restart → `--apply-update`.
- Mode detection for deb, AUR and a read-only AppImage.
- `release_e2e.py` runs in no CI workflow.

**9.12**
- The join dialog's error states (invalid code, central down, an auth error) and its close / cancel.

**9.13**
- No request while the window is focused.

## 3. What only the owner can check

**7.1, 7.4, 7.7, 7.8**: the two-person listening tests already written in HANDOFF §6:
- "Interface sounds, side by side with Electron";
- "The listening test for the click fixes", steps 1–9;
- per-user volume and mute heard;
- a server-muted member's stream keeps its sound.

**7.5**: global push to talk while a game has focus.

**7.6**: with "Default" chosen, change the OS's default output in voice: the session follows within a moment. Unplug the chosen headset in voice: voice moves to the default.

**7.11**: the GlobalShortcuts portal on KDE and GNOME Wayland (the confirmation dialog, then the bound combo with another app focused). Also a German layout under KDE: does the portal bind the key pressed? The core builds `preferred_trigger` from US keysym names (`core/hotkeys/keys.rs:441-461`; low confidence).

**8.1, 8.2**: a real Go Live through the portal here (NVENC). Then the encoder checks on AMD and Intel (`crates/decibell-core/tools/verify-encoders.md`).

**8.3**: share "Only these" with two apps playing. Viewers hear exactly the ticked ones, never Decibell.

**8.4**: by eye, the HANDOFF §6 list ("Stream playback on AMD and Intel, and by eye"): colours, smoothness, a 120 fps stream, the fullscreen cursor under Wayland and XWayland.

**8.6**: a viewer on another machine sees our thumbnail at the start, at +2 s, then every 15 s.

**8.7**: with "Take stream with me" on, switch channels while sharing: the share continues in the new channel for the viewers there.

**8.8**: two machines in an MLS channel, a stream with sound: both play. A viewer whose keys are locked gets the passphrase prompt instead of the channel.

**9.11**: `PACKAGING.md` §11 (the signing key, a real tag run, the AppImage tool hashes, NSIS update and rollback).

**9.12**: a `decibell://invite/…` clicked in a browser with the package installed: the window comes forward with the join dialog (HANDOFF §6, desktop integration item 1).

**9.13**: the X11 urgency hint on a real X11 session.

**9.14**: a real Sentry project receives a panic and a native crash; a Windows SEH record.

## 4. Self-tests run (2026-11-27)

**How they were run.** The first batch called the binary directly, one test at a time. After the coordinator's instruction, everything failing was rerun through `gpui-client/tools/selftest.sh` (one test window on the machine at a time).

**KWin access.** `attention` and `single-instance` find KWin's helper through `tools/perf` among the binary's ancestors. The frozen root has no `tools/` directory, so there `attention` logged SKIP (exit 0) and `single-instance` skipped its compositor check. They were run from a private root of hard links to the same two binaries, plus a `tools` symlink (`~/.cache/decibell-dev/scratch/parity-c/root`). Both passed with KWin. **Anyone running the platform tests from `parity-root` gets the SKIP.**

**PASS:**
- Voice and calls: `voice-join`, `voice-switch`, `voice-mute`, `voice-cues`, `voice-golive-dialog`, `voice-watch`, `voice-stall`, `call-ring`.
- Video and encode: `encoder-software`, `video-probe`, `video-hw`, `video-sw`, `video-two`, `video-idle`, `video-recover`, `video-switch`, `video-nvdec`, `video-vaapi`, `video-order`, `video-starve`, `video-stall`, `video-fastpath`.
- Settings: `settings-open`, `settings-appearance-apply`, `settings-devices-signout`, `settings-password`, `settings-rename`, `settings-privacy-dms`, `settings-e2ee`, `settings-audio-meter`, `settings-keybinds-record`, `settings-network-caps`, `settings-idle`, `settings-narrow`.
- Hotkeys and platform: `hotkeys-focused`, `hotkeys-layout`, `hotkey-cli`, `deep-link-arg`, `single-instance` (KWin), `attention` (KWin), `window-state`, `idle-x11`.
- Shell, content and the rest: `servers-join-link`, `shell-signout`, `shell-navigate`, `content-embed`, `user-menu-moderation`, `picture-editor`, `gif-picker`, `regional`, `themes`.
- `packaging/tests/release_e2e.py --binary <frozen>`: 14 of 14, loopback fakes only, run under the self-test lock.

**Failed three times or more:**
- **`settings-unsaved-shake`**, 5 of 5 (2 alone under the wrapper's lock):
  - four times: "a press on another tab with a typed value didn't open it";
  - once: "still drawing after 15 s" after the Esc shake.
  - Gap 1.5.
- **`render-cost`**, 4 of 4 (1 under the wrapper): "only 0 of 30 changes drew a frame".
  - This is its third case, a key typed in a field under the message list.
  - Its two voice cases measured normally: a speaking change on a 12-tile stage costs 5.2 views rendered and 13 replayed with patch 0014, against 18 rendered without it.
  - It also fails on the Windows runner (run 45984832437).
  - It belongs to the chat / composer rows, not these.
- **`voice-speaking-idle`**, 3 of 5 (2 of 3 under the wrapper): "the voice view drew while nothing on screen changed: try 4: 9 drawn, 19 presented in 10 s (unfocused) … 0 latency changes". It passed twice. Gap 1.6.

**Null sinks.**
- One run of mine (`call-ring`, stopped mid-test when I switched to the wrapper) left `decibell-selftest-294349`. I unloaded it (module 536870917, named in that run's log). The default devices were unchanged.
- **`decibell-selftest-295055` (module 536870916) is still loaded and is not mine**: no log of mine names it, and its process is gone. It appeared around 12:23.

## 5. Windows

**What ran on the Windows runner** (run 45984832437 on `86c1b34`):
- PASS: `encoder-software` (8.2), `video-probe` (8.4: D3D11VA absent on WARP, software decode drawn), `themes` (9.3), `kit-modal` (9.10).
- FAIL: `render-cost`.

**Rows with no step in `docs/windows-owner-checks.md`:**
- §7: 7.1 joining, leaving and switching voice; 7.2 rosters; 7.3 the stage (§3 covers stream tiles only); 7.4 WASAPI capture with AEC / NS / AGC; 7.7 per-user volume; 7.8 mute, deafen and moderation; 7.9 MLS voice; 7.10 the stats popover.
- §8: 8.6 thumbnails sent from DXGI / WGC capture; 8.7 take stream with me. 8.8 has no step for stream sound.
- §9: 9.1 Account (including the avatar once it exists); 9.2 Devices (the OS name Windows reports); 9.3 Appearance; 9.7 Network (`%USERPROFILE%\Downloads` ignores a relocated Downloads folder); 9.9 About and 9.11 auto-update (only `PACKAGING.md` §11 lists the NSIS run); 9.14 crash reports (an SEH record and its next-start send).

## 6. HANDOFF §1b (Electron's main-process jobs)

Every owner the table names exists in code. Corrections:
- **`window.ts` `hardenNavigation`**: "links open through `external_link_target`" is not true for message links. `N/decibell-chat/src/pane.rs:166` and `N/decibell-home/src/dm/conversation.rs:291` call `cx.open_url(url)` directly, with no scheme check or length cap. It is safe today only because autolinks are http(s) or invites.
- **`index.ts` `before-quit`**: Restart to update bypasses `quit_guard` (gap 1.16).
- **`dialog.ts`**: the missing type filters aren't in electron-quirks (gap 1.18), and the picture pickers' `accept` is missing.
- **`createWindow`**: "dark background, shown when ready" has no counterpart. Whether a light first frame flashes is for the owner's eye.
- **Missing from the table**:
  - Electron's default application menu (no `Menu.setApplicationMenu`): Ctrl+R reload, Ctrl+Shift+I devtools, F11 fullscreen, Ctrl+W / Ctrl+Q, zoom. Unverified whether they reach the frameless window.
  - The preload's stream-frame routing (owner exists: `N/decibell-app/src/video.rs`).
  - `addon.ts`'s `session_token_updated` → attachment targets (owner exists: `MD/events/chat.rs:349`).

## 7. Electron features outside FEATURES §1–§9 that the native client lacks

Spellcheck and zoom are known (HANDOFF §1b).
- **Drop files on a text channel's row in the sidebar.** The row pulses, the upload goes to that channel, and the row shows a progress bar (`R/features/channels/ServerChannelsSidebar.tsx:829-953`, `R/features/chat/useDragDrop.ts:70-115`). M.
- **Screen readers.** Chromium's accessibility tree plus the renderer's `aria-label`s; the native client sets no roles or labels. L.
- **Tab through server tiles and channel, DM and member rows** (buttons in Electron). M.
- **The OS's reduce-motion setting.** The theme reads a flag nothing sets from the OS. S–M.
- **Drag text or a link from another app into the composer.** Files only today; needs a GPUI patch. M.
- **Select and copy fixed text** (profile fields, safety numbers, fingerprint, device address, keybind commands). S each.
- **Alt+↑/↓ to reorder roles.** The native tooltip still promises it (`N/decibell-server-settings/src/server/roles.rs:1009`). S.

## 8. Wrong outside these rows

**Both clients:**
- Both show a raw uid during a call: "Call · 12345" in the user panel (`R/features/channels/UserPanel.tsx:85`, `N/decibell-shell/src/user_panel.rs:176-177`; `call_peer` is a `Uid` in both). This breaks "people never see uids".
- Both keep the local stream "live" after the server refuses `START_STREAM` (mirrored).

**Native client:**
- Attachment sound ignores the owner's device rule of 2026-11-26. CLAUDE.md says attachments follow the stream-output device when that is on, else the voice output device. `N/decibell-media/src/output.rs:1-5,56-60` always opens the system default, and `docs/electron-quirks.md` #19 still says "Mirrors". (Row 6.7.)
- Small render-cost nits: the dock redraws on every settings change, including each volume-slider tick (`V/dock.rs:111-115`); the panel popovers redraw on every own-stream change through a branch that does nothing (`V/menus.rs:329-336`).

**Electron client:**
- The mini-player's title shows the streamer's raw uid (`R/features/voice/MiniStreamPlayer.tsx:474`). The native client shows the name. Record it in electron-quirks.

**Docs:**
- `docs/FEATURES.md` §9 says crash reports are opt-in with an opt-in banner. Both clients are opt-out: on by default (`core/config.rs:146-151`), and the banner is a one-way disclosure.
- `docs/FEATURES.md` 8.4 says "per-stream volume". Both clients have one global stream-audio gain.
- `docs/electron-quirks.md` #14 calls the 500 ms keyframe-request throttle "the renderer's". The renderer's is 1 s (`StreamVideoPlayer.tsx:131`); 500 ms is the core's.
- The stats overlay's amber "Queue > 3" no longer fits the native client's 100 ms queue: it is amber routinely at 60 / 120 fps.
- HANDOFF §1 (the Settings row) still lists the mic test outside voice and the updater as left. Both are built; only the avatar editor is left.
- Stale comments:
  - `S/tabs/privacy.rs:8-10`, `S/tabs/about.rs:3-4` and `MD/stores/update.rs:1-2` each say "a later task";
  - `N/decibell-app/src/audio.rs:153` has `TODO(attention)`, but attention is built;
  - `N/decibell-platform/src/attention.rs:6` names patch 0015 (it is 0017);
  - `N/decibell-app/src/boot.rs:333` says "until Settings → Appearance exists".
- `settings-narrow` is both a user-Settings screen and a Server-Settings self-test. `CONVENTIONS.md` says the two share one namespace; `registry.rs` treats them as two.

**Tests:**
- The model's invite grammar (the deep-link path, `MD/invite_link.rs`) is the one copy of three not checked against the TypeScript vectors.
- Test bodies that assert less than they say:
  - `voice-mute`'s comment says "Undeafen first", but it clicks mute (`V/selftests.rs:690-698`);
  - `voice-join`'s "the peer's tile goes" checks the roster, not the tile;
  - `hotkey-cli`'s "not focused" check is only a missing marker.
