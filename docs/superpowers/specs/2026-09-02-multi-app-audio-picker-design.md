# Multi-App Audio Picker — Design

**Date:** 2026-09-02
**Author:** sunkhan (with Claude)
**Status:** Implemented on `main` (`cbaabec`, `a29293e`, `763b2d7`); Linux and Win10 live tests pending

## Problem

"Share audio" on the Go Live dialog was a single boolean. On Windows it meant
"the picked window's process" (window source) or "everything except Decibell"
(screen source); on Linux it always meant "everything except Decibell" because
the XDG portal hands over an opaque video node with no window identity; on
macOS and on the renderer-WebCodecs path there is no stream audio at all.
Streamers could not choose *which* applications a stream carries — a game, a
music player and a Discord call all went out together.

## Decisions

| Question | Decision |
|---|---|
| Rule shape | Three modes over one ticked set: **Selected apps** (whitelist), **All except** (blacklist), **All apps** (previous behaviour; checkboxes disabled). The mode only reinterprets the set. |
| Live edits | Yes. A speaker button next to Stop (UserPanel, VoicePanel, own-stream card in a DM call) opens the same picker as a popover; a tick persists **and** is pushed to the running capture (`set_stream_audio_filter`). |
| Window sources | The picker fully owns the audio rule for window *and* screen sources; the old Windows "window → that app only" auto-scoping is gone. The app owning the picked window is flagged "this window" in the list. Default mode is All apps. |
| App identity | A stable, platform-local, lowercase program name — never a PID. Windows: exe stem (`chrome`, `spotify`); Linux: `application.process.binary`, else `application.name`, else `node.name`. Persisted in the settings blob (`stream_audio_mode`, `stream_audio_apps`). |
| What is listed | Any app with an open audio session (Windows) / `Stream/Output/Audio` node (Linux), playing or paused, with a live dot on the ones rendering now. Decibell's own process tree is never listed or captured. Remembered ticks for apps not running render greyed so a stale one can be cleared. |
| Where it shows | Only where stream audio is native: Windows always, Linux with a hardware encoder. Hidden on macOS and on the WebCodecs fallback (`canPickStreamAudioApps()`; native also reports `supported`). |
| Wire | None. `StartStreamRequest.has_audio` stays a bool; the filter is local to the streamer. Selected-with-nothing-ticked streams silence and the UI says so. |
| Blacklist vs process trees (Windows) | A WASAPI process-loopback client can only include a whole tree, so a blacklisted process *inside* an allowed parent's tree (a game launched by Steam) cannot be excluded on its own: the parent is suppressed too and logged. A blacklist prefers silence over a leak. Whitelist mode does not suppress — the user ticked the parent. |

## Non-goals (v1)

- macOS stream audio (there is none today; the picker hides).
- Stream audio on the renderer-WebCodecs path (the audio track from
  `getDisplayMedia` is still dropped).
- Event-driven session discovery (`IAudioSessionNotification`, PipeWire
  registry events) — both platforms poll every 2 s and wake immediately on a
  filter change.
- Friendly Windows display names from the exe's `FileDescription`; v1 shows
  the exe stem in its original casing.
- Per-app volume; icons.

## Architecture

```
renderer                                   native
────────                                   ──────
StreamAudioAppPicker ──poll 2 s──▶ list_stream_audio_apps ──▶ Linux: pw-dump nodes (+Client fallback)
   │ ticks / mode                                             Windows: IAudioSessionManager2 sessions
   ▼
streamAudioFilter.ts ─▶ voiceStore.streamSettings.{audioMode,audioApps} ─▶ saveSettings (config.json)
   │ while live
   └──▶ set_stream_audio_filter ──▶ AppState.stream_audio_filter + AudioStreamEngine::set_filter
                                         │
Go Live ─▶ start_screen_share{audioMode,audioApps} ─▶ capture backend starts with the filter
                                         ▼
                        Linux PipewireTap ─┐             Windows ProcessLoopbackMixer ─┐
                        (link/unlink taps) │  AudioFrame  (N include clients or 1 exclude-self,
                                           └─────────────▶ summed by Mixer)              │
                                           audio_stream_pipeline → Opus → STREAM_AUDIO ◀─┘
```

### Filter model (`native/src/media/stream_audio_filter.rs`, cfg-free)

`StreamAudioMode { Selected, AllExcept, All }` (serde `snake_case`, the wire /
config spelling), `StreamAudioFilter { mode, apps: BTreeSet<String> }` with
`from_args` (lenient: unknown mode → All, identities normalised),
`allows(identity)`, `is_pass_through()` (All, or AllExcept with nothing
ticked). `normalize_identity` / `identity_from_exe_path` are the one spelling
rule; the renderer mirrors it in `normalizeAppId`.

The trait `StreamAudioCapture { set_filter }` is the handle
`AudioStreamEngine` owns (replacing the Linux-only cleanup closure): drop =
full teardown, `set_filter` must return fast (the caller holds the AppState
lock) and do the work on the capture's own thread.

Windows planning is pure and tested here on Linux: `plan_clients(filter,
sessions, parent_of, self_pid, self_identity) → ExcludeSelf |
Include { pids, suppressed }` — pass-through → one exclude-self client;
otherwise the allowed session PIDs deduped to tree roots
(`dedup_tree_roots`; an include-tree client already covers descendants), with
the AllExcept leak-safety rule above. `diff_pids` turns two sets into
add/remove lists. `group_sessions` builds the picker rows.

### Engine + commands

- `AudioStreamEngine::start(rx, socket, sender, bitrate, capture:
  Option<Box<dyn StreamAudioCapture>>)`; `set_filter`; `stop()` shuts the
  pipeline first (frame receiver drops → capture threads see Disconnected),
  then drops the capture handle.
- `AppState::stream_audio_filter` remembers the filter between streams.
- `commands/stream_audio.rs`: `list_stream_audio_apps { sourceId? } → {
  supported, apps: [{ id, name, pids, active, ownsWindowSource }] }` and
  `set_stream_audio_filter { mode, apps }` (store + poke; always Ok).
- `StartScreenShareArgs.audio_mode / audio_apps` (both absent → the stored
  filter).

### Linux (`native/src/media/capture_audio_pipewire.rs`)

`parse_app_nodes(pw-dump)` reads identity / PID / `application.name` /
`info.state` from each `Stream/Output/Audio` node, falling back to its
**Client** object (`client.id` → `pipewire.sec.pid`,
`application.process.binary`). That fallback also fixes a real leak:
Decibell's own CPAL output (`alsa_playback.decibell`) carries no PID on the
Node, so the old "PID-less = not us" rule tapped the voice chat into the
stream. `is_self` = PID in our `/proc` tree **or** identity equals our own
binary stem.

`reconcile_taps` does one dump per pass: `pw-link <port> <sink-port>` for
every output port of an allowed node, `pw-link -d <link-id>` for every link
into the capture sink whose source is no longer allowed. The poller is a
`recv_timeout(2 s)` on a wake channel; `PipewireTap::set_filter` swaps the
filter and wakes it, so a tick applies within milliseconds. Dropping the
`PipewireTap` joins the poller and unloads the null sink (which drops every
tap link).

Multi-node apps (Firefox exposes one node per audio context) collapse into
one row by identity. Wine games all share `wine-preloader` as their binary —
a known v1 limitation.

### Windows (`native/src/media/capture_audio_wasapi.rs`, `stream_audio_mixer.rs`)

One `IAudioClient` on `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK` covers exactly
one process tree, so `ProcessLoopbackMixer` runs a *set* of clients:

- `decibell-audio-mixer` thread: reads the default render endpoint's mix
  format **once** (the virtual device has no `GetMixFormat`), owns the
  clients and the cfg-free `Mixer`, ticks every 10 ms, makes no COM call
  inside the tick. Planning input arrives as messages: `SetFilter` (re-plan
  now + wake the scanner), `Snapshot` (re-plan), `Shutdown`.
- `decibell-audio-scan` thread: `EnumAudioEndpoints(eRender, ACTIVE)` →
  `IAudioSessionManager2` → sessions (skip PID 0, `IsSystemSoundsSession() ==
  S_OK` — **S_FALSE means "no", never `.is_ok()`** — expired sessions, our own
  exe path; identity via `QueryFullProcessImageNameW`) plus a Toolhelp
  parent-PID map; every 2 s and on wake. New Cargo feature:
  `Win32_System_Diagnostics_ToolHelp`.
- `decibell-audio-src-<pid>` threads: the existing activate / `Initialize(…,
  LOOPBACK, 20 ms, shared format)` / 10 ms `GetBuffer` loop, pushing stereo
  f32 chunks keyed by PID. A thread that ends (target exited, device
  invalidated, activation refused) is reaped on the next tick and re-added by
  the next snapshot; one that dies within a second goes on a 15 s cooldown.
- `Mixer`: per-source ring, wall-clock paced (`frames_due` with integer
  nanosecond math; a backlog beyond the 200 ms cap is forgiven, not burst),
  20 ms prime before a source contributes, zero-pad a short source, un-prime
  an empty one, trim a source running ahead at 200 ms, clamp ±1.0. `None`
  when no primed source has data, so an idle desktop costs no bandwidth.

`list_apps(source_id)` (own COM thread) groups sessions by identity, flags
the app owning a `window:HWND:0` source and appends it inactive if it has no
session yet, so a silent-so-far game can still be ticked.

### Renderer

- Types: `StreamAudioMode` const-enum, `StreamAudioApp`, `StreamAudioAppList`
  (`src/types/index.ts`).
- `voiceStore.streamSettings.{audioMode, audioApps}` ↔
  `stream_audio_mode` / `stream_audio_apps` in `saveSettings` / `loadSettings`
  (validated, normalised, deduped, capped at 256).
- `features/voice/streamAudioFilter.ts` is the one place that persists **and**
  pokes: `setStreamAudioMode`, `toggleStreamAudioApp`,
  `canPickStreamAudioApps`, `normalizeAppId`.
- `stores/streamAudioAppsStore.ts` polls `list_stream_audio_apps` (in-flight
  coalescing; the set is skipped when the list is shallow-equal).
- `features/voice/StreamAudioAppPicker.tsx` (mode `SegmentedControl` — now
  in `src/components/` — + checkbox rows), embedded in `CaptureSourcePicker`
  under the bitrate control; `StreamAudioPopover.tsx` (`StreamAudioButton`)
  next to Stop. `StreamCapture` forwards `audioMode` / `audioApps` and exposes
  `activeStreamSourceId()` for the popover's "this window" flag.

## Verification

```sh
cd electron-client && npx tsc -p tsconfig.web.json --noEmit          # 0 errors
cd electron-client/native && npx napi build --platform --js index.js --dts index.d.ts
cd electron-client/native && cargo test --lib                          # 131/131 (was 104)
gh workflow run win-native-check.yml --ref <branch>                    # green on a29293e
```

**Pending live** (see the manual matrix in the plan):

- Linux, native encode: two apps playing; each mode; tick while live
  (change within ~1 s); app start / quit mid-stream; `pw-link -l` shows no
  `alsa_playback.decibell → decibell_capture` link (the leak fix); null sink
  gone after Stop; software-encode fallback hides the picker.
- Windows: the three modes with a screen source; "this window" on a window
  source (Chrome's audio-service child shares the exe → same identity);
  Chrome with two tabs → one row; kill an app mid-stream → source reaped,
  no engine error, re-added on relaunch; Steam + game blacklist → both
  silent + log line; default output device switch mid-stream; Win10 2004 +
  Win11; channel move keeps the filter.

## Rollout

Client-only; no server change. Older configs load with `audioMode = all`
and no ticks, i.e. exactly the previous behaviour on screen sources. The one
behaviour change is deliberate: a Windows *window* source no longer scopes
audio to that app by itself — tick it (it's flagged) or leave All apps.

## Known limitations / follow-ups

- Whether process loopback captures a process rendering to a *non-default*
  endpoint is unverified; document "audio played to the default output" if
  it doesn't.
- ≤ 2 s until a newly started app is picked up in Selected / All-except
  mode (the blacklist is an include-set by construction, so a new *allowed*
  app is missed briefly rather than a blacklisted one leaking). Follow-up:
  `IAudioSessionNotification::OnSessionCreated` / PipeWire registry events.
- The shared mix format is read once; after a default-device change new
  clients may fail `Initialize` until the stream restarts (logged, retried
  each snapshot). Follow-up: request a fixed 48 kHz stereo f32 format as the
  Microsoft ApplicationLoopback sample does, which also makes the pipeline's
  resampler pass-through.
- Wall-clock vs audio-clock drift shows up as a rare 200 ms trim.
