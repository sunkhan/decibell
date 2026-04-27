# Video Codec Negotiation — Design Spec

**Date:** 2026-04-27
**Status:** Approved for implementation planning
**Scope:** Streaming pipeline (video). Audio (Opus) is unchanged.
**Platform focus:** Windows. Linux receive path remains the existing JPEG-transcode and is explicitly out of scope; Linux send is in scope.

---

## 1. Goal

Replace the current single-codec (H.264 hardware) streaming path with a four-codec system — AV1, H.265/HEVC, H.264 hardware, H.264 software (x264) — and add a peer-negotiated, server-mediated capability exchange so streamers automatically pick the best codec their viewers can decode. Add streamer enforcement of a specific codec, with viewer gating in the UI when they cannot decode it.

VP9 is removed. The legacy `CODEC_VP9 = 0` slot in the wire-format enum becomes `CODEC_UNKNOWN = 0`. The C++ community server is already codec-agnostic (it forwards UDP video packets without inspecting the codec byte) and requires no relay-path changes.

---

## 2. Vocabulary

- **Capabilities** — the set of codecs (with per-codec resolution/fps ceilings) a client can encode and a separate set it can decode. Encode and decode are independent.
- **Probe** — runtime detection of available encoders (FFmpeg encoder init+teardown) and decoders (`VideoDecoder.isConfigSupported` in WebCodecs). Run once at first launch, cached to disk.
- **`original_settings`** — the codec/resolution/fps the streamer chose at stream start. Acts as the high-water mark for upgrade-back.
- **`current_settings`** — what the streamer is actively encoding right now.
- **`target_settings`** — what `current_settings` should be, given the constraints of all current watchers. Recomputed on every watch event.
- **LCD (lowest common denominator)** — the codec/resolution/fps that satisfies every current watcher's decode caps and the streamer's encode caps.
- **Enforcement** — `enforced_codec` set on the stream. Disables auto-negotiation; viewers without the codec see a grayed-out watch button.

---

## 3. Capability format and probing

### 3.1 Wire format

```protobuf
enum VideoCodec {
  CODEC_UNKNOWN = 0;   // legacy VP9 slot, retired
  CODEC_H264_HW = 1;   // preserves existing wire value
  CODEC_H264_SW = 2;   // x264 software
  CODEC_H265 = 3;      // HEVC, hardware
  CODEC_AV1 = 4;       // hardware
}

message CodecCapability {
  VideoCodec codec = 1;
  uint32 max_width = 2;
  uint32 max_height = 3;
  uint32 max_fps = 4;
}

message ClientCapabilities {
  repeated CodecCapability encode = 1;
  repeated CodecCapability decode = 2;
}
```

Encode and decode lists are independent. An RTX 3060 user typically advertises encode `[H.265, H.264_HW, H.264_SW]` and decode `[AV1, H.265, H.264]` — they cannot encode AV1 (no NVENC AV1 block) but their NVDEC handles AV1 decode, exposed through WebCodecs.

### 3.2 Policy ceilings

These are conservative defaults applied during probing. They cap the values written into `CodecCapability` even if the encoder/decoder claims more.

| Codec | Encode ceiling | Decode ceiling |
|---|---|---|
| AV1 | 3840×2160 @ 60 | 3840×2160 @ 60 |
| H.265 | 3840×2160 @ 60 | 3840×2160 @ 60 |
| H.264 hardware | 2560×1440 @ 60 | 3840×2160 @ 60 |
| H.264 software (x264) | 1920×1080 @ 60 | — (covered by H.264 decode) |

There is no "x264 decode" entry — any H.264 decoder handles any valid H.264 stream regardless of which encoder produced it.

### 3.3 Probing

- **Encode probe** — for each codec candidate, attempt to construct an FFmpeg encoder context with a tiny test config; on success, record the codec with its policy ceiling. Order tried per codec: NVENC → AMF → QSV → MF (Windows) → x264 software (last, only for H.264_SW).
- **Decode probe** — call `VideoDecoder.isConfigSupported({ codec: 'av01.0.05M.08' })` (and equivalent for `hev1.1.6.L120.B0`, `avc1.640033`); on success, record the codec with its policy ceiling. WebCodecs is the source of truth for decode because it transparently surfaces hardware decode block availability.
- **When** — once on first launch. Result written to `caps.json` in the app data directory. Reused on every subsequent launch.
- **Manual refresh** — "Refresh codec capabilities" button in Settings → Codecs re-runs the probe and rewrites the file. Also re-emits the new caps to any connected community server (sends a fresh `JoinVoiceRequest`-equivalent capability update — exact mechanics in §4.6).
- **User toggles** — the "Use AV1 codec when available" and "Use H.265/HEVC codec when available" settings are filters applied on top of the probe result. Disabled toggle → that codec is removed from the encode list before advertisement. Decode list is not filtered (decoding is cheap; no reason to refuse incoming streams).

---

## 4. Signaling flows

All signaling is over the existing TCP/TLS channel to the community server.

### 4.1 Voice channel join

Client sends `JoinVoiceRequest { channel_id, capabilities }`. Server stores `capabilities` on the session struct.

### 4.2 Voice presence broadcast

`VoicePresenceUpdate` (already broadcast on every voice channel state change) gains `repeated ClientCapabilities user_capabilities`, parallel to the existing `user_states`. Every voice-channel member receives every other member's capabilities and caches them locally as `username → ClientCapabilities`.

### 4.3 Stream start

`StartStreamRequest` gains `enforced_codec` (default `CODEC_UNKNOWN` = no enforcement) and `chosen_codec` (the codec the streamer is starting with — auto-picked or = `enforced_codec`). Server stores both on the per-stream registry entry. Resolution/fps in the request are pre-clamped by the streamer to the chosen codec's encode ceiling.

### 4.4 Stream presence broadcast

`VideoStreamInfo` (inside `StreamPresenceUpdate`) gains `current_codec` and `enforced_codec`. Viewers compute locally `can_decode = current_codec ∈ local_decode_caps && (enforced_codec == 0 || enforced_codec ∈ local_decode_caps)` to decide whether to enable the Watch button.

### 4.5 Mid-stream codec change

New packet type `STREAM_CODEC_CHANGED_NOTIFY = 58` carrying:

```protobuf
message StreamCodecChangedNotify {
  string channel_id = 1;
  string streamer_username = 2;
  VideoCodec new_codec = 3;
  uint32 new_width = 4;
  uint32 new_height = 5;
  uint32 new_fps = 6;
  enum Reason {
    REASON_UNKNOWN = 0;
    WATCHER_JOINED_LOW_CAPS = 1;
    LIMITING_WATCHER_LEFT = 2;
    STREAMER_INITIATED = 3;
  }
  Reason reason = 7;
}
```

Streamer sends this when it switches codec. Server validates the sender owns the stream, updates the registry entry, rebroadcasts `StreamPresenceUpdate` so all clients get the new `current_codec`. The per-packet `codec` byte in subsequent `UdpVideoPacket` packets is the wire authority for each individual packet — viewers' WebCodecs decoders see the byte change and reconfigure themselves to match.

### 4.6 Capability refresh mid-session

When the user clicks "Refresh codec capabilities" while connected, the client must re-emit its capabilities to the community server so other clients learn the new caps. Two viable mechanics:

- **(a)** New message `UpdateCapabilities { capabilities }` that the server stores and rebroadcasts via `VoicePresenceUpdate`.
- **(b)** Force-rejoin the voice channel transparently (leave + join with new caps).

Pick **(a)** for minimal user-visible disruption. Implementation note: this is also what the streamer would use if a future "Refresh" happened mid-stream.

---

## 5. Codec selection algorithm

The streamer maintains:
- `original_settings`: `(codec, width, height, fps)` chosen at stream start. Immutable for the session.
- `current_settings`: what's actually encoding now.
- `enforced_codec`: 0 if not enforced.

### 5.1 LCD picker (used everywhere)

Given a set of watchers `W` and the streamer's encode caps:

1. Take the encoder priority list `[AV1, H.265, H.264_HW, H.264_SW]`, filtered by user toggles (Settings → Codecs).
2. For each candidate codec `C` in priority order:
   - `C` must be in streamer's encode caps.
   - `C` must be in `w.decode_caps` for every `w ∈ W`.
   - If both hold:
     - `width = min(original.width, streamer.encode[C].max_width, min over w ∈ W of w.decode[C].max_width)`
     - `height = min` analogous
     - `fps = min` analogous
     - Return `(C, width, height, fps)`.
3. If no codec satisfies all watchers (extremely unlikely in practice — H.264 decode is universal in WebCodecs), pick the highest-priority codec satisfying the most watchers; the server-side defensive check in §4.5 will have rejected the incompatible watchers' `WatchStreamRequest` already.

### 5.2 At stream start

- If `enforced_codec` is set: `current_settings = (enforced_codec, clamped_dims)`. Skip auto-pick.
- Else: `current_settings = lcd(W = ∅, …)` — with no watchers, the LCD picker just returns the highest-priority codec the streamer can encode (filtered by user toggles), clamped to the streamer's chosen resolution/fps.
- `original_settings = current_settings`.

### 5.3 On `WatchStreamRequest` arrival

- Look up the new watcher's caps in the local presence cache.
- Compute `target = lcd(existing watchers ∪ {new watcher}, …)`.
- If `target == current_settings`: nothing to do.
- Else if `target` is a downgrade (lower-priority codec or smaller dims): start the **200 ms downgrade debounce** timer. If another watch event fires before the timer expires, reset the timer with the recomputed `target`. When the timer fires, swap to `target` (§6) and send `StreamCodecChangedNotify` with `reason = WATCHER_JOINED_LOW_CAPS`.

### 5.4 On `StopWatchingRequest` or watcher leaves voice

- Compute `target = lcd(remaining watchers, …)`.
- If `target == current_settings`: nothing to do.
- Else if `target` is an upgrade (higher-priority codec or larger dims): start the **30 second upgrade cooldown** timer. If any watch event during the cooldown causes a downgrade recompute (`target` becomes equal to or lower than `current_settings`), cancel the cooldown — we're staying. When the cooldown fires, swap to `target` and send `StreamCodecChangedNotify` with `reason = LIMITING_WATCHER_LEFT`.

### 5.5 Enforcement

When `enforced_codec != CODEC_UNKNOWN`:
- §5.2/§5.3/§5.4 are short-circuited; no auto-changes.
- Server defensively rejects `WatchStreamRequest` from watchers whose caps lack `enforced_codec`. Client UI already grays the button (§7.3); this is belt-and-suspenders.
- `STREAMER_INITIATED` reason exists for future "streamer toggles enforcement mid-stream" — not in v1 UI, but reserved.

---

## 6. Mid-stream switch mechanics

The actual swap operation, executed when a debounce/cooldown timer fires:

1. Build new encoder for `target_settings`. This is the slow step — typically 100–300 ms for hardware encoders, longer for software x264. During this window the old encoder keeps producing packets normally (no freeze yet).
2. On new encoder ready: force a keyframe on the new encoder.
3. Atomic swap: subsequent encode calls go to the new encoder. Old encoder is torn down on a background task (no need to block the pipeline thread).
4. The first packets out of the new encoder carry the new `codec` byte. Viewers' WebCodecs decoders see the byte change, tear down the old `VideoDecoder`, and configure a new one with the appropriate codec string. Realistic viewer-side reconfigure: 50–100 ms.
5. The `StreamCodecChangedNotify` is sent at step 1 (before the swap completes) so the toast shows just before the visual transition — UX feels intentional rather than reactionary.

Total realistic freeze for existing viewers: 200–500 ms with a toast explaining why.

The 200 ms downgrade debounce in §5.3 coalesces simultaneous watcher joins — if three H.264-only viewers click watch in the same frame, the streamer recomputes `target` three times but only swaps once.

The 30 s upgrade cooldown in §5.4 prevents thrashing if a low-cap viewer pops in and out repeatedly; the second downgrade after a recent upgrade would still cause a glitch but at least the sequence is not "downgrade — upgrade — downgrade" within a few seconds.

---

## 7. UI surface

### 7.1 Settings → new "Codecs" panel

- Toggle: "Use AV1 codec when available" (default: on if probe found AV1 encode).
- Toggle: "Use H.265/HEVC codec when available" (default: on if probe found H.265 encode).
- Each toggle is grayed with the explanation "Your hardware does not support this codec" if the probe found nothing for that codec.
- Button: "Refresh codec capabilities" — re-runs probe, rewrites `caps.json`, sends `UpdateCapabilities` to any connected community server.
- Read-only summary: "Detected encoders: AV1 (NVENC), H.264 (NVENC, software). Decoders: AV1, H.265, H.264." — so the user can see what was probed.

### 7.2 Stream-start dialog (`StreamConfigDialog`)

Adds one new control to the existing dialog:
- Dropdown labelled "Codec" with options: `Auto (recommended)` (default) plus one entry per codec the streamer can encode after settings filtering — `Force AV1`, `Force H.265`, `Force H.264`, `Force H.264 (software)`. Codecs the streamer cannot encode are not listed.
- Selecting anything other than "Auto" populates `enforced_codec` on the `StartStreamRequest`.
- Tooltip on the dropdown: "Forcing a codec prevents viewers without that decoder from watching this stream."

### 7.3 Stream presence list — watch button gating

For each entry in `StreamPresenceUpdate.active_streams`:
- Compute `can_decode = current_codec ∈ local_decode_caps && (enforced_codec == 0 || enforced_codec ∈ local_decode_caps)`.
- `can_decode == true` → normal Watch button.
- `can_decode == false` → grayed-out unclickable Watch button. Hover tooltip: "Cannot decode {codec_name} — your hardware/browser doesn't support it." If enforced, append " (streamer has locked this codec)".

### 7.4 Stream player badge

A pill-shaped badge in the top-right corner of the video tile:
- Format: `1080p60 · AV1` (resolution + fps + codec name).
- Color tier suggestive of codec generation: AV1 = purple, H.265 = blue, H.264 hardware = teal, H.264 software = gray. Final colors via design pass.
- Lock icon (FontAwesome ``) appears next to the codec name when `enforced_codec != CODEC_UNKNOWN`. Hover tooltip on the lock: "Streamer has locked this stream to {codec_name}".
- The badge is driven off the `current_codec` and `enforced_codec` fields in the latest `StreamPresenceUpdate` for that stream — updates live when codec changes.

### 7.5 Toast notifications

Driven by `StreamCodecChangedNotify.reason`:
- Streamer-side:
  - `WATCHER_JOINED_LOW_CAPS` → "Switched to {new_codec} at {res}{fps} so {watcher_username} can watch."
  - `LIMITING_WATCHER_LEFT` → "Restored to {new_codec} at {res}{fps}."
- Watcher-side:
  - `WATCHER_JOINED_LOW_CAPS` → "{streamer} switched to {new_codec} ({res}{fps})."
  - `LIMITING_WATCHER_LEFT` → "{streamer} restored to {new_codec} ({res}{fps})."
- Position: top-center of the voice channel view. Duration: 4 seconds. Style: matches existing toast/notification system; if no such system exists, the implementation plan adds a minimal one.

---

## 8. Quality presets, codec-aware

The existing Low / Medium / High / Custom quality tiers translate to bitrate via a per-codec bits-per-pixel-per-second multiplier. `bitrate_kbps = bpp_s × width × height × fps / 1000`.

| Quality | H.264 (HW & SW) | H.265 / HEVC | AV1 |
|---|---|---|---|
| Low    | 0.020 | 0.013 | 0.010 |
| Medium | 0.050 | 0.033 | 0.025 |
| High   | 0.080 | 0.054 | 0.040 |

Custom bypasses the table — the user-supplied bitrate is used as-is regardless of codec switches.

When a stream auto-switches codec (§5.3, §5.4), the bitrate is recomputed from this table for the new (codec, resolution, fps) so that visual quality stays roughly comparable across the switch. This avoids the case where 5 Mbps AV1 (looks great) becomes 5 Mbps H.264 (looks bad) on downgrade.

The numbers are first-pass and tweakable later without protocol changes — bitrate is a streamer-local computation.

---

## 9. Wire-protocol summary checklist

**Proto** (`proto/messages.proto`):

| Item | Change |
|---|---|
| `enum VideoCodec` | new — `CODEC_UNKNOWN=0` (reclaims VP9 slot), `CODEC_H264_HW=1`, `CODEC_H264_SW=2`, `CODEC_H265=3`, `CODEC_AV1=4` |
| `message CodecCapability` | new — `codec, max_width, max_height, max_fps` |
| `message ClientCapabilities` | new — `repeated CodecCapability encode, decode` |
| `JoinVoiceRequest` | add `ClientCapabilities capabilities = 2` |
| `VoicePresenceUpdate` | add `repeated ClientCapabilities user_capabilities = 4` |
| `StartStreamRequest` | add `VideoCodec enforced_codec = 7`, `VideoCodec chosen_codec = 8` |
| `VideoStreamInfo` | add `VideoCodec current_codec = 7`, `VideoCodec enforced_codec = 8` |
| `Packet.Type STREAM_CODEC_CHANGED_NOTIFY = 58` | new packet type |
| `Packet.Type UPDATE_CAPABILITIES_REQ = 59` | new packet type for §4.6 |
| Payload oneof entries | add `stream_codec_changed_notify = 60`, `update_capabilities_req = 61` |
| `message StreamCodecChangedNotify` | new — see §4.5 |
| `message UpdateCapabilitiesRequest` | new — `ClientCapabilities capabilities` |

**UDP** (`src/common/udp_packet.hpp` + Rust mirror in `tauri-client/src-tauri/src/media/video_packet.rs`):
- `UdpVideoPacket.codec` byte: no struct change. Allowed values extended to match proto enum.
- C++ enum updated: remove `CODEC_VP9`, add `CODEC_UNKNOWN=0, CODEC_H264_HW=1, CODEC_H264_SW=2, CODEC_H265=3, CODEC_AV1=4`. Rust constants likewise.

**Server** (`src/community/main.cpp`):
- Store `ClientCapabilities` on session.
- Read `JoinVoiceRequest.capabilities` and the new `UpdateCapabilitiesRequest`; store on session and rebroadcast presence.
- Populate `VoicePresenceUpdate.user_capabilities`.
- Store `current_codec` and `enforced_codec` on per-stream registry entry from `StartStreamRequest`. Update on `StreamCodecChangedNotify`. Always populate them in `StreamPresenceUpdate`.
- Handle `STREAM_CODEC_CHANGED_NOTIFY`: validate sender owns stream, update registry, rebroadcast presence.
- Defensively reject `WatchStreamRequest` if `enforced_codec != 0` and watcher lacks the codec.
- The UDP relay path (`broadcast_to_watchers`) is unchanged — server stays codec-agnostic on the data path.

**Tauri Rust** (`tauri-client/src-tauri/src/media/`):
- `encoder.rs`: add HEVC (`hevc_nvenc`/`hevc_amf`/`hevc_qsv`/`hevc_mf`), AV1 (`av1_nvenc`/`av1_amf`/`av1_qsv`), x264 software (`libx264`) encoder branches.
- New `caps.rs`: probe encoders + decoders, persist to `caps.json` in app data dir, expose `get_caps()` and `refresh_caps()`.
- New `codec_selection.rs`: LCD picker, debounce/cooldown timers, swap orchestration.
- `video_pipeline.rs`: integrate the swap operation (§6).
- Cargo: ensure FFmpeg build pulls in libx264 and the chosen AV1 encoder paths (libsvtav1 / hardware variants); add features as needed.

**Tauri React** (`tauri-client/src/`):
- New Settings → "Codecs" panel.
- `StreamConfigDialog`: new "Codec" enforce dropdown.
- `StreamVideoPlayer`: WebCodecs decoder config branch on codec — `avc1.640033`, `hev1.1.6.L120.B0`, `av01.0.05M.08`. Reconfigure when per-packet codec byte changes.
- `StreamViewPanel` / stream tile: badge with codec/res/fps, lock icon when enforced, grayed-out watch button with tooltip when can't decode.
- `voiceStore`: store `userCapabilities: Record<username, ClientCapabilities>` from `VoicePresenceUpdate`; expose helper `canDecode(codec)`.
- Toast component for codec-change notifications.

---

## 10. Out of scope (deferred)

- Linux receive-path optimization (currently H.264 → JPEG transcode in Rust). Will be addressed in a follow-up.
- Simulcast / overlap during codec switch — explicitly rejected during brainstorming in favor of the simpler hard-switch approach.
- Streamer mid-stream toggling of `enforced_codec` — the `STREAMER_INITIATED` reason is reserved on the wire but no UI ships in v1.
- Runtime measurement of encoder throughput (was option (c) in capability shape — explicitly rejected for the simpler static-policy approach).
- Adaptive bitrate algorithm changes beyond the codec-aware preset table. The existing NACK-ratio adaptive bitrate logic continues to operate on top of the codec-aware preset.
- Audio codec negotiation — Opus stays.

---

## 11. Notes for the implementation plan

The implementation order that minimizes risk:

1. **Wire format first** — proto changes, regenerate, verify compile across Rust, C++, and JS bindings. No behavior change yet.
2. **Server-side capability storage and rebroadcast** — sessions store caps, presence broadcasts include them. Clients still hardcode H.264 on send.
3. **Client capability probing** — `caps.rs` + `caps.json`, send caps in `JoinVoiceRequest`. Settings panel UI.
4. **New encoder backends in Rust** — HEVC, AV1, x264. Verified one at a time with a forced-codec dev path before touching selection logic.
5. **Codec selection algorithm + swap mechanics** — `codec_selection.rs` and `video_pipeline.rs` integration. Test with two clients (one full-cap, one H.264-only).
6. **WebCodecs decoder branching in React** — handle codec byte changes mid-stream.
7. **Stream-start enforcement + UI** — dropdown, server-side rejection, watch button gating.
8. **Badge, lock icon, toasts** — polish layer.

Each step is independently testable and shippable behind the existing wire-format additions (which are backward-compatible additions to existing messages).
