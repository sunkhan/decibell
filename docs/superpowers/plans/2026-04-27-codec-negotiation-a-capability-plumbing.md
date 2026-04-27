# Codec Negotiation — Plan A: Capability Plumbing

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Plumb a new `ClientCapabilities` (encode + decode codec lists with per-codec resolution/fps caps) end-to-end through the wire format, server, Rust client, and React UI. No streaming behavior changes; H.264 hardware remains the only codec actually used. This plan establishes the foundation for Plans B and C.

**Architecture:** Adds new proto messages and field extensions, syncs the UDP `VideoCodec` enum across C++ and Rust, introduces a Rust `caps.rs` module that probes FFmpeg encoders, a React WebCodecs decoder probe, a Tauri command bridge to merge them, persistent caches (`encoder_caps.json` on disk, decoder caps in `localStorage`), and a Settings → Codecs panel with on/off toggles plus a refresh button. The community server stores per-session capabilities and includes them in `VoicePresenceUpdate` broadcasts. The new `chosen_codec` and `enforced_codec` fields on `StartStreamRequest`/`VideoStreamInfo` are stored and rebroadcast but unused by the streamer (always set to `CODEC_H264_HW`).

**Tech Stack:** Protobuf 3, prost-build (Rust), `protobuf_generate_cpp` (CMake), Tauri v2 commands, Zustand (React state), WebCodecs API, FFmpeg via `ffmpeg-next` 8.

**Spec reference:** `docs/superpowers/specs/2026-04-27-video-codec-negotiation-design.md` §§ 3, 4.1, 4.2, 4.6, 7.1, 9.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `proto/messages.proto` | Modify | All new enum, messages, field additions |
| `src/common/udp_packet.hpp` | Modify | C++ `VideoCodec` enum sync |
| `tauri-client/src-tauri/src/media/video_packet.rs` | Modify | Rust UDP codec constants sync |
| `tauri-client/src-tauri/src/media/caps.rs` | Create | Encoder probe + persistence + merged-caps builder |
| `tauri-client/src-tauri/src/media/mod.rs` | Modify | `pub mod caps;` |
| `tauri-client/src-tauri/src/commands/settings.rs` | Modify | New Tauri commands: `get_caps`, `refresh_caps`, `set_decoder_caps`, `get_codec_settings`, `set_codec_settings` |
| `tauri-client/src-tauri/src/commands/voice.rs` | Modify | Attach `capabilities` to `JoinVoiceRequest` build |
| `tauri-client/src-tauri/src/state.rs` | Modify | Hold decoder caps + codec toggle settings in app state |
| `tauri-client/src-tauri/src/config.rs` | Modify | Persist codec toggle settings in user config file |
| `src/community/main.cpp` | Modify | Store `ClientCapabilities` on session, populate `VoicePresenceUpdate.user_capabilities`, store and rebroadcast `current_codec`/`enforced_codec` on streams |
| `tauri-client/src/types/index.ts` | Modify | Hand-written TS types matching new proto messages |
| `tauri-client/src/utils/decoderProbe.ts` | Create | WebCodecs `VideoDecoder.isConfigSupported` probe |
| `tauri-client/src/stores/voiceStore.ts` | Modify | Cache `userCapabilities: Record<username, ClientCapabilities>` |
| `tauri-client/src/stores/codecSettingsStore.ts` | Create | Zustand store for the two toggle states |
| `tauri-client/src/features/settings/CodecsPanel.tsx` | Create | Settings panel UI: toggles + refresh button + probe summary |
| `tauri-client/src/features/settings/SettingsModal.tsx` (or equivalent) | Modify | Wire CodecsPanel into the settings modal navigation |
| `tauri-client/src/main.tsx` (or App.tsx) | Modify | At app boot, run decoder probe and ship to Rust via `set_decoder_caps` |

---

## Task 1: Add new VideoCodec enum and capability messages to proto

**Files:**
- Modify: `proto/messages.proto`

- [ ] **Step 1: Add VideoCodec enum and CodecCapability + ClientCapabilities messages**

In `proto/messages.proto`, add at the top of the file just after `package chatproj;` (so the enum and supporting messages are defined before they're referenced):

```protobuf
// --- Video codec negotiation ---
// Wire-compatible with the legacy single-codec setup: CODEC_H264_HW = 1
// preserves the value the existing Tauri client has been stamping in
// UdpVideoPacket.codec since launch. CODEC_UNKNOWN = 0 reclaims the
// retired CODEC_VP9 slot — server has always been codec-agnostic on
// the relay path so the change has no on-wire impact.
enum VideoCodec {
  CODEC_UNKNOWN = 0;
  CODEC_H264_HW = 1;
  CODEC_H264_SW = 2;
  CODEC_H265 = 3;
  CODEC_AV1 = 4;
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

- [ ] **Step 2: Verify Rust regen compiles**

Run from `tauri-client/src-tauri/`:
```bash
cargo check
```
Expected: compiles with warnings only — the new types are present in `chatproj.rs` (in OUT_DIR) but unused.

- [ ] **Step 3: Verify C++ regen compiles**

Run from the build directory (e.g., `build-servers/`):
```bash
cmake --build . --target chatproj_common
```
Expected: compiles. The new `chatproj::VideoCodec`, `chatproj::CodecCapability`, `chatproj::ClientCapabilities` types are now generated in `messages.pb.h`.

- [ ] **Step 4: Commit**

```bash
git add proto/messages.proto
git commit -m "proto: add VideoCodec enum + CodecCapability/ClientCapabilities messages"
```

---

## Task 2: Add capability fields to existing proto messages

**Files:**
- Modify: `proto/messages.proto`

- [ ] **Step 1: Add capabilities to JoinVoiceRequest**

Replace the `JoinVoiceRequest` message:

```protobuf
message JoinVoiceRequest {
  string channel_id = 1;
  // Capabilities of the joining client. Empty when sent by an older
  // client that pre-dates negotiation; server treats that as "H.264
  // decode/encode only, conservative ceilings" for back-compat.
  ClientCapabilities capabilities = 2;
}
```

- [ ] **Step 2: Add user_capabilities to VoicePresenceUpdate**

Replace the `VoicePresenceUpdate` message:

```protobuf
message VoicePresenceUpdate {
  string channel_id = 1;
  repeated string active_users = 2;
  repeated VoiceUserState user_states = 3;
  // Parallel to user_states/active_users — same length, same order.
  // user_capabilities[i] belongs to active_users[i].
  repeated ClientCapabilities user_capabilities = 4;
}
```

- [ ] **Step 3: Add codec fields to StartStreamRequest**

Replace the `StartStreamRequest` message:

```protobuf
message StartStreamRequest {
  string channel_id = 1;
  int32 target_fps = 2;
  int32 target_bitrate_kbps = 3;
  bool has_audio = 4;
  uint32 resolution_width = 5;
  uint32 resolution_height = 6;
  // Codec the streamer is starting with. Auto-picked from the streamer's
  // encode caps (filtered by user toggles) when enforced_codec is UNKNOWN.
  VideoCodec chosen_codec = 7;
  // CODEC_UNKNOWN = no enforcement, auto-negotiation enabled.
  // Anything else = streamer locks the stream to this codec; viewers
  // without it see a grayed-out watch button (computed locally).
  VideoCodec enforced_codec = 8;
}
```

- [ ] **Step 4: Add codec fields to VideoStreamInfo**

Replace the `VideoStreamInfo` message:

```protobuf
message VideoStreamInfo {
  string stream_id = 1;
  string owner_username = 2;
  bool has_audio = 3;
  uint32 resolution_width = 4;
  uint32 resolution_height = 5;
  uint32 fps = 6;
  // Live current codec. Changes mid-stream when the streamer renegotiates.
  VideoCodec current_codec = 7;
  // CODEC_UNKNOWN if the streamer chose Auto. Drives the lock badge
  // and the grayed-out watch button.
  VideoCodec enforced_codec = 8;
}
```

- [ ] **Step 5: Verify regen compiles in Rust and C++**

```bash
# from tauri-client/src-tauri/
cargo check
# from build-servers/
cmake --build . --target chatproj_common
```
Both expected to compile.

- [ ] **Step 6: Commit**

```bash
git add proto/messages.proto
git commit -m "proto: extend JoinVoiceRequest, VoicePresenceUpdate, StartStreamRequest, VideoStreamInfo with capability + codec fields"
```

---

## Task 3: Add new packet types and notify messages

**Files:**
- Modify: `proto/messages.proto`

- [ ] **Step 1: Add new Packet.Type enum entries**

In `proto/messages.proto`, find the `Packet.Type` enum and add (placing after the last existing `CHANNEL_WIPED = 57;`):

```protobuf
    // Video codec negotiation.
    STREAM_CODEC_CHANGED_NOTIFY = 58;  // streamer→server→all in channel
    UPDATE_CAPABILITIES_REQ = 59;      // client→community when caps refreshed mid-session
```

- [ ] **Step 2: Add the two payload oneof entries**

In `Packet.payload` oneof, add at the end (after `channel_wiped = 59;`):

```protobuf
    StreamCodecChangedNotify stream_codec_changed_notify = 60;
    UpdateCapabilitiesRequest update_capabilities_req = 61;
```

- [ ] **Step 3: Add the two new message definitions**

Append to `proto/messages.proto`:

```protobuf
// Sent by the streamer when it switches codec/resolution/fps mid-stream.
// Server validates the sender owns the stream, updates the registry
// entry, and rebroadcasts StreamPresenceUpdate to the channel.
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

// Sent by a client mid-session when the user clicks "Refresh codec
// capabilities" in Settings. Server stores the new caps on the session
// and rebroadcasts VoicePresenceUpdate so peers see the change without
// the user having to leave and rejoin the voice channel.
message UpdateCapabilitiesRequest {
  ClientCapabilities capabilities = 1;
}
```

- [ ] **Step 4: Verify regen compiles in Rust and C++**

```bash
# from tauri-client/src-tauri/
cargo check
# from build-servers/
cmake --build . --target chatproj_common
```
Both expected to compile.

- [ ] **Step 5: Commit**

```bash
git add proto/messages.proto
git commit -m "proto: add STREAM_CODEC_CHANGED_NOTIFY and UPDATE_CAPABILITIES_REQ packet types"
```

---

## Task 4: Sync UDP VideoCodec enum in C++ and Rust

**Files:**
- Modify: `src/common/udp_packet.hpp`
- Modify: `tauri-client/src-tauri/src/media/video_packet.rs`

The on-wire `UdpVideoPacket.codec` byte must mirror the proto enum. Existing Tauri streamer stamps value 1 = H.264; we keep that meaning by renaming `CODEC_H264` → `CODEC_H264_HW` in both header and Rust constants.

- [ ] **Step 1: Update C++ enum**

In `src/common/udp_packet.hpp`, replace lines 31-34:

```cpp
enum VideoCodec : uint8_t {
    CODEC_UNKNOWN = 0,   // legacy VP9 slot, retired — server has always ignored this byte on relay
    CODEC_H264_HW = 1,   // hardware H.264 (NVENC/AMF/QSV/MF) — preserves existing wire value
    CODEC_H264_SW = 2,   // x264 software encoder
    CODEC_H265 = 3,      // HEVC, hardware
    CODEC_AV1 = 4        // AV1, hardware
};
```

Also update the comment on line 84 of the same file (`UdpVideoPacket.codec`):

```cpp
    uint8_t codec;                      // VideoCodec: see enum above
```

- [ ] **Step 2: Update Rust constants**

In `tauri-client/src-tauri/src/media/video_packet.rs`, replace line 15:

```rust
pub const CODEC_UNKNOWN: u8 = 0;
pub const CODEC_H264_HW: u8 = 1;
pub const CODEC_H264_SW: u8 = 2;
pub const CODEC_H265: u8 = 3;
pub const CODEC_AV1: u8 = 4;
```

Then update the references in the same file:
- Line 96: `codec: CODEC_H264,` → `codec: CODEC_H264_HW,`
- Line 291 (in `tests`): `assert_eq!(codec, CODEC_H264);` → `assert_eq!(codec, CODEC_H264_HW);`

- [ ] **Step 3: Find and update any other references to the old name**

Search for stragglers:
```bash
grep -rn "CODEC_H264\b" src/ tauri-client/src-tauri/src/ src/common/
grep -rn "CODEC_VP9" src/ tauri-client/src-tauri/src/ src/common/
```

Expected: no results after replacement. Update any straggler to the new name.

- [ ] **Step 4: Run Rust tests**

```bash
# from tauri-client/src-tauri/
cargo test --lib video_packet
```
Expected: all tests in `video_packet.rs` pass — the size_matches_cpp test still produces 1445 bytes (struct unchanged), the codec value test now compares against CODEC_H264_HW.

- [ ] **Step 5: Build C++ servers**

```bash
# from build-servers/
cmake --build . --target chatproj_community chatproj_server
```
Expected: builds. (No behavior change yet — the enum just got renamed.)

- [ ] **Step 6: Commit**

```bash
git add src/common/udp_packet.hpp tauri-client/src-tauri/src/media/video_packet.rs
git commit -m "udp: rename VideoCodec values to match proto (CODEC_H264_HW preserves wire value)"
```

---

## Task 5: Hand-write TypeScript types for the new proto messages

**Files:**
- Modify: `tauri-client/src/types/index.ts`

The JS side has no proto codegen (verified — no `ts-proto`/`protobufjs` in package.json). All proto-shaped types are hand-written in camelCase.

- [ ] **Step 1: Add VideoCodec, CodecCapability, ClientCapabilities types**

Append to `tauri-client/src/types/index.ts`:

```typescript
// Mirrors the VideoCodec enum in proto/messages.proto. Numeric values
// must match the wire — they are also the byte stamped in UdpVideoPacket.codec.
export const VideoCodec = {
  UNKNOWN: 0,
  H264_HW: 1,
  H264_SW: 2,
  H265: 3,
  AV1: 4,
} as const;
export type VideoCodec = (typeof VideoCodec)[keyof typeof VideoCodec];

export interface CodecCapability {
  codec: VideoCodec;
  maxWidth: number;
  maxHeight: number;
  maxFps: number;
}

export interface ClientCapabilities {
  encode: CodecCapability[];
  decode: CodecCapability[];
}
```

- [ ] **Step 2: Extend the StreamInfo type with the new codec fields**

Find the `StreamInfo` interface (line ~96) and add at the end before the closing brace:

```typescript
  fps: number;
  currentCodec: VideoCodec;
  enforcedCodec: VideoCodec;  // VideoCodec.UNKNOWN = no enforcement
```

(Keep the existing `resolutionWidth`, `resolutionHeight` fields.)

- [ ] **Step 3: Extend the VoiceParticipant type**

Find `VoiceParticipant` and add capabilities — note we keep capabilities as a separate map in the store, but adding it on the participant type is also valid. We'll keep it on the store for cleanliness; no change to `VoiceParticipant` here, just confirm it's not needed by reading it.

(Skip if no change needed.)

- [ ] **Step 4: Run TypeScript check**

```bash
# from tauri-client/
npm run build
```
(or `npx tsc --noEmit` if `npm run build` does too much.)
Expected: passes — the new types are self-contained, existing usages of `StreamInfo` will need to be updated in later tasks but are not yet broken because TypeScript will assume new fields are missing on existing data, which is fine for the build step.

If type errors appear from existing call sites that destructure `StreamInfo`, add `currentCodec: VideoCodec.UNKNOWN, enforcedCodec: VideoCodec.UNKNOWN` defaults in the construction sites until later tasks populate them.

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src/types/index.ts
git commit -m "types: add VideoCodec, CodecCapability, ClientCapabilities, extend StreamInfo with codec fields"
```

---

## Task 6: Create the Rust caps module — encoder probe scaffold

**Files:**
- Create: `tauri-client/src-tauri/src/media/caps.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

- [ ] **Step 1: Add module declaration**

In `tauri-client/src-tauri/src/media/mod.rs`, add (alongside the other `pub mod` lines):

```rust
pub mod caps;
```

- [ ] **Step 2: Create caps.rs scaffold with types**

Create `tauri-client/src-tauri/src/media/caps.rs`:

```rust
//! Codec capability probing and persistence.
//!
//! Encoders are probed via FFmpeg (try to construct a codec context for each
//! candidate). Decoders are probed in the React layer using WebCodecs and
//! shipped here via the `set_decoder_caps` Tauri command. The merged
//! `ClientCapabilities` is what gets sent over the wire.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::RwLock;

/// Wire-compatible numeric values from proto/messages.proto VideoCodec enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CodecKind {
    Unknown = 0,
    H264Hw = 1,
    H264Sw = 2,
    H265 = 3,
    Av1 = 4,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodecCap {
    pub codec: CodecKind,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ClientCaps {
    pub encode: Vec<CodecCap>,
    pub decode: Vec<CodecCap>,
}

/// Process-wide cache of the most recently probed/loaded encoder caps.
/// Decoder caps live in app state (set from JS).
static ENCODER_CACHE: RwLock<Option<Vec<CodecCap>>> = RwLock::new(None);

fn caps_path() -> PathBuf {
    // Same dir convention used elsewhere in the project; if a helper exists
    // (e.g. crate::config::app_data_dir), prefer that.
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("decibell");
    let _ = std::fs::create_dir_all(&dir);
    dir.join("encoder_caps.json")
}
```

- [ ] **Step 3: Add `dirs` dependency**

In `tauri-client/src-tauri/Cargo.toml` `[dependencies]`:

```toml
dirs = "5"
```

If the project already uses a different home/config-dir helper (check `crate::config` for an `app_data_dir()` function), use that instead and skip this step.

- [ ] **Step 4: Verify it compiles**

```bash
cargo check
```
Expected: compiles with `dead_code` warnings on the unused functions/types.

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src-tauri/src/media/caps.rs tauri-client/src-tauri/src/media/mod.rs tauri-client/src-tauri/Cargo.toml
git commit -m "feat(caps): scaffold caps module with CodecKind/CodecCap/ClientCaps types"
```

---

## Task 7: Implement the encoder probe

**Files:**
- Modify: `tauri-client/src-tauri/src/media/caps.rs`

The encoder probe attempts to construct an FFmpeg encoder context for each candidate. Success → record the codec with its policy ceiling; failure → skip.

- [ ] **Step 1: Write the probe function**

Append to `tauri-client/src-tauri/src/media/caps.rs`:

```rust
/// (codec, list of FFmpeg encoder name candidates in priority order)
fn encoder_candidates() -> Vec<(CodecKind, Vec<&'static str>)> {
    vec![
        (CodecKind::Av1, vec!["av1_nvenc", "av1_amf", "av1_qsv"]),
        (CodecKind::H265, vec!["hevc_nvenc", "hevc_amf", "hevc_qsv", "hevc_mf"]),
        (CodecKind::H264Hw, vec!["h264_nvenc", "h264_amf", "h264_qsv", "h264_mf"]),
        (CodecKind::H264Sw, vec!["libx264"]),
    ]
}

/// Per-codec encode policy ceilings (spec §3.2). These cap the values
/// written into CodecCap regardless of what FFmpeg/the GPU could probably
/// handle in practice — keeps reliability defensible.
fn encode_ceiling(codec: CodecKind) -> (u32, u32, u32) {
    match codec {
        CodecKind::Av1 => (3840, 2160, 60),
        CodecKind::H265 => (3840, 2160, 60),
        CodecKind::H264Hw => (2560, 1440, 60),
        CodecKind::H264Sw => (1920, 1080, 60),
        CodecKind::Unknown => (0, 0, 0),
    }
}

/// Try to construct a tiny FFmpeg encoder context for the given codec name.
/// Returns true if the encoder is usable on this machine.
fn probe_one_encoder(name: &str) -> bool {
    use ffmpeg_next as ffmpeg;
    let codec = match ffmpeg::encoder::find_by_name(name) {
        Some(c) => c,
        None => return false,
    };
    // Build a small encoder context. We do not actually feed frames —
    // construction + open() is enough to verify the encoder initializes.
    let ctx = match ffmpeg::codec::context::Context::new_with_codec(codec) {
        c => c,
    };
    let mut enc = match ctx.encoder().video() {
        Ok(e) => e,
        Err(_) => return false,
    };
    enc.set_width(640);
    enc.set_height(360);
    enc.set_format(ffmpeg::format::Pixel::NV12);
    enc.set_time_base((1, 30));
    enc.set_frame_rate(Some((30, 1)));
    enc.set_bit_rate(1_000_000);
    enc.open_as(codec).is_ok()
}

/// Probe all candidate encoders. Returns a list of CodecCap entries —
/// one per codec for which at least one backend opened successfully.
pub fn probe_encoders() -> Vec<CodecCap> {
    let mut out = Vec::new();
    for (kind, names) in encoder_candidates() {
        let mut found = false;
        for name in &names {
            if probe_one_encoder(name) {
                found = true;
                eprintln!("[caps] encoder available: {:?} via {}", kind, name);
                break;
            }
        }
        if found {
            let (w, h, fps) = encode_ceiling(kind);
            out.push(CodecCap { codec: kind, max_width: w, max_height: h, max_fps: fps });
        }
    }
    out
}
```

- [ ] **Step 2: Add load/save helpers**

Append:

```rust
pub fn load_cached_encoders() -> Option<Vec<CodecCap>> {
    let path = caps_path();
    let data = std::fs::read(&path).ok()?;
    serde_json::from_slice::<Vec<CodecCap>>(&data).ok()
}

pub fn save_encoders(caps: &[CodecCap]) {
    let path = caps_path();
    if let Ok(data) = serde_json::to_vec_pretty(caps) {
        let _ = std::fs::write(&path, data);
    }
}

/// Get encoder caps: load from cache if present, otherwise probe and cache.
pub fn get_or_probe_encoders() -> Vec<CodecCap> {
    if let Some(c) = ENCODER_CACHE.read().ok().and_then(|g| g.clone()) {
        return c;
    }
    let caps = match load_cached_encoders() {
        Some(c) => c,
        None => {
            let probed = probe_encoders();
            save_encoders(&probed);
            probed
        }
    };
    if let Ok(mut g) = ENCODER_CACHE.write() {
        *g = Some(caps.clone());
    }
    caps
}

/// Force re-probe — used by the "Refresh codec capabilities" button.
pub fn refresh_encoders() -> Vec<CodecCap> {
    let probed = probe_encoders();
    save_encoders(&probed);
    if let Ok(mut g) = ENCODER_CACHE.write() {
        *g = Some(probed.clone());
    }
    probed
}
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo check
```
Expected: compiles. Some `dead_code` warnings still ok.

- [ ] **Step 4: Add a unit test that the probe runs without panicking**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_encoders_does_not_panic() {
        let caps = probe_encoders();
        // We don't assert any particular codec is present — depends on
        // the build's FFmpeg config. We just check the call returns.
        eprintln!("probed encoders: {:?}", caps);
        // H264_SW (libx264) should always be available if the FFmpeg build
        // has it. Plan B will ensure that. For now, the probe might return
        // empty on a minimal FFmpeg build and that's fine for this task.
        let _ = caps;
    }
}
```

- [ ] **Step 5: Run the test**

```bash
cargo test --lib caps::tests::probe_encoders_does_not_panic -- --nocapture
```
Expected: passes. Eprintln output shows which encoders are available on the dev machine.

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/media/caps.rs
git commit -m "feat(caps): implement encoder probe + JSON cache"
```

---

## Task 8: Hold decoder caps and codec settings in app state

**Files:**
- Modify: `tauri-client/src-tauri/src/state.rs`

The decoder probe runs in JS (WebCodecs is unavailable in Rust). React ships the result via a Tauri command, and we cache it in app state so the next `JoinVoiceRequest` build can read it.

- [ ] **Step 1: Read the existing state.rs to understand the AppState pattern**

```bash
cat tauri-client/src-tauri/src/state.rs | head -80
```

Familiarize with how state fields are currently held (likely a struct stored as `tauri::State<AppState>`).

- [ ] **Step 2: Add decoder caps and codec settings fields**

In `tauri-client/src-tauri/src/state.rs`, add to the `AppState` struct (or equivalent):

```rust
use crate::media::caps::CodecCap;
use std::sync::RwLock;

// ... inside AppState struct
    /// Decoder caps shipped from React via set_decoder_caps. Updated on
    /// app boot and again whenever the user clicks "Refresh codec
    /// capabilities" in Settings.
    pub decoder_caps: RwLock<Vec<CodecCap>>,

    /// User's codec preference toggles. Filters the encode list before
    /// the LCD picker runs. Persisted in user config (see config.rs).
    pub use_av1: RwLock<bool>,
    pub use_h265: RwLock<bool>,
```

Initialize them in the `AppState::new()` (or `Default`) impl:

```rust
            decoder_caps: RwLock::new(Vec::new()),
            use_av1: RwLock::new(true),   // default-on per spec §7.1
            use_h265: RwLock::new(true),
```

(If the existing pattern uses `tokio::sync::RwLock` instead of `std::sync::RwLock`, follow that.)

- [ ] **Step 3: Verify it compiles**

```bash
cargo check
```
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/state.rs
git commit -m "state: hold decoder caps and codec preference toggles"
```

---

## Task 9: Persist codec settings in user config

**Files:**
- Modify: `tauri-client/src-tauri/src/config.rs`

- [ ] **Step 1: Read config.rs to see the existing config struct + persistence pattern**

```bash
cat tauri-client/src-tauri/src/config.rs
```

- [ ] **Step 2: Add the two fields to the config struct**

Add to the persistent config struct in `config.rs` (with sensible defaults matching spec §7.1):

```rust
    #[serde(default = "default_true")]
    pub use_av1: bool,
    #[serde(default = "default_true")]
    pub use_h265: bool,
```

Add the helper if not already present:

```rust
fn default_true() -> bool { true }
```

- [ ] **Step 3: Wire load/save**

If config loading already happens at app boot, on load: write the values into `AppState.use_av1` / `use_h265`. On save: read from app state.

If you find this pattern in `lib.rs` or `main.rs` (look for `Config::load()` or similar), follow that exact pattern.

- [ ] **Step 4: Verify compiles**

```bash
cargo check
```

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src-tauri/src/config.rs tauri-client/src-tauri/src/lib.rs
git commit -m "config: persist use_av1/use_h265 codec preference toggles"
```

---

## Task 10: Tauri commands for caps + settings

**Files:**
- Modify: `tauri-client/src-tauri/src/commands/settings.rs`
- Modify: `tauri-client/src-tauri/src/lib.rs` (or wherever invoke_handler is)

- [ ] **Step 1: Add the command implementations**

In `tauri-client/src-tauri/src/commands/settings.rs`, add (using the existing `#[tauri::command]` and `tauri::State` patterns):

```rust
use crate::media::caps::{self, CodecCap, ClientCaps, CodecKind};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Serialize)]
pub struct CapsResponse {
    pub encode: Vec<CodecCap>,
    pub decode: Vec<CodecCap>,
}

/// Returns the merged ClientCapabilities the next JoinVoiceRequest would
/// send. React uses this to render the Codecs settings panel summary.
#[tauri::command]
pub fn get_caps(state: State<AppState>) -> CapsResponse {
    let encode = caps::get_or_probe_encoders();
    let decode = state.decoder_caps.read().map(|g| g.clone()).unwrap_or_default();
    // Apply user toggles to the encode list (spec §3.3).
    let use_av1 = *state.use_av1.read().unwrap();
    let use_h265 = *state.use_h265.read().unwrap();
    let filtered_encode = encode
        .into_iter()
        .filter(|c| match c.codec {
            CodecKind::Av1 => use_av1,
            CodecKind::H265 => use_h265,
            _ => true,
        })
        .collect();
    CapsResponse { encode: filtered_encode, decode }
}

/// Re-runs the encoder probe and returns the new merged caps.
#[tauri::command]
pub fn refresh_caps(state: State<AppState>) -> CapsResponse {
    caps::refresh_encoders();
    get_caps(state)
}

/// React calls this at app boot (and again after a refresh) with its
/// WebCodecs probe result.
#[tauri::command]
pub fn set_decoder_caps(state: State<AppState>, decoder_caps: Vec<CodecCap>) {
    if let Ok(mut g) = state.decoder_caps.write() {
        *g = decoder_caps;
    }
}

#[derive(Serialize, Deserialize)]
pub struct CodecSettings {
    pub use_av1: bool,
    pub use_h265: bool,
}

#[tauri::command]
pub fn get_codec_settings(state: State<AppState>) -> CodecSettings {
    CodecSettings {
        use_av1: *state.use_av1.read().unwrap(),
        use_h265: *state.use_h265.read().unwrap(),
    }
}

#[tauri::command]
pub fn set_codec_settings(state: State<AppState>, settings: CodecSettings) {
    *state.use_av1.write().unwrap() = settings.use_av1;
    *state.use_h265.write().unwrap() = settings.use_h265;
    // Persist to disk — call into the config helper that already handles
    // the existing settings (e.g., crate::config::save_with(|c| { ... })).
    // If the project has an "on every state change save" pattern, follow
    // that. Otherwise add an explicit save here.
}
```

- [ ] **Step 2: Register the commands in the invoke_handler**

In `tauri-client/src-tauri/src/lib.rs` (or wherever `tauri::generate_handler!` is), add the four new command names alongside the existing ones:

```rust
            commands::settings::get_caps,
            commands::settings::refresh_caps,
            commands::settings::set_decoder_caps,
            commands::settings::get_codec_settings,
            commands::settings::set_codec_settings,
```

- [ ] **Step 3: Verify compiles**

```bash
cargo check
```
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/commands/settings.rs tauri-client/src-tauri/src/lib.rs
git commit -m "feat(commands): get_caps/refresh_caps/set_decoder_caps/get_codec_settings/set_codec_settings"
```

---

## Task 11: WebCodecs decoder probe in React

**Files:**
- Create: `tauri-client/src/utils/decoderProbe.ts`

- [ ] **Step 1: Write the probe utility**

Create `tauri-client/src/utils/decoderProbe.ts`:

```typescript
import { VideoCodec, type CodecCapability } from "../types";

// Decode policy ceilings (spec §3.2).
const DECODE_CEILING: Record<number, { width: number; height: number; fps: number }> = {
  [VideoCodec.AV1]:   { width: 3840, height: 2160, fps: 60 },
  [VideoCodec.H265]:  { width: 3840, height: 2160, fps: 60 },
  [VideoCodec.H264_HW]: { width: 3840, height: 2160, fps: 60 },
};

// WebCodecs codec strings used to probe support. These are conservative
// "well-known" profile/level codes; actual streams may use slightly
// different parameters but the decoder configuration accepts any valid
// stream of the same family once the AVCC/HEVC/AV1 description is supplied.
const PROBE_CONFIGS: { codec: VideoCodec; webCodecsString: string }[] = [
  { codec: VideoCodec.AV1,    webCodecsString: "av01.0.05M.08" },
  { codec: VideoCodec.H265,   webCodecsString: "hev1.1.6.L120.B0" },
  { codec: VideoCodec.H264_HW, webCodecsString: "avc1.640033" },
];

const LOCAL_STORAGE_KEY = "decibell.decoder_caps.v1";

export async function probeDecoders(force = false): Promise<CodecCapability[]> {
  if (!force) {
    const cached = localStorage.getItem(LOCAL_STORAGE_KEY);
    if (cached) {
      try { return JSON.parse(cached); } catch { /* fall through to probe */ }
    }
  }

  const out: CodecCapability[] = [];

  // WebCodecs may not exist (very old webview, or non-Chromium). In that
  // case we still report H.264 decode — spec §3.3 H.264 fallback.
  const VideoDecoderCtor: typeof VideoDecoder | undefined =
    typeof VideoDecoder !== "undefined" ? VideoDecoder : undefined;

  for (const { codec, webCodecsString } of PROBE_CONFIGS) {
    let supported = false;
    if (VideoDecoderCtor) {
      try {
        const res = await VideoDecoderCtor.isConfigSupported({
          codec: webCodecsString,
          hardwareAcceleration: "prefer-hardware",
        });
        supported = !!res.supported;
      } catch {
        supported = false;
      }
    }
    if (supported) {
      const ceiling = DECODE_CEILING[codec];
      out.push({
        codec,
        maxWidth: ceiling.width,
        maxHeight: ceiling.height,
        maxFps: ceiling.fps,
      });
    }
  }

  // H.264 fallback (spec §3.3) — always advertise H.264 decode even if
  // probe didn't confirm. Worst case the WebCodecs decoder fails to
  // configure on a real stream and the player shows an error, but the
  // LCD picker can always converge.
  if (!out.some((c) => c.codec === VideoCodec.H264_HW)) {
    out.push({
      codec: VideoCodec.H264_HW,
      maxWidth: DECODE_CEILING[VideoCodec.H264_HW].width,
      maxHeight: DECODE_CEILING[VideoCodec.H264_HW].height,
      maxFps: DECODE_CEILING[VideoCodec.H264_HW].fps,
    });
  }

  localStorage.setItem(LOCAL_STORAGE_KEY, JSON.stringify(out));
  return out;
}

export function clearDecoderCache(): void {
  localStorage.removeItem(LOCAL_STORAGE_KEY);
}
```

- [ ] **Step 2: Wire decoder probe at app boot**

In `tauri-client/src/main.tsx` (or `App.tsx` if that's where boot logic lives), after the existing app initialization:

```typescript
import { invoke } from "@tauri-apps/api/core";
import { probeDecoders } from "./utils/decoderProbe";

// Probe + ship caps to Rust as soon as the React app mounts.
// This must complete before the user joins a voice channel.
probeDecoders().then((caps) => {
  invoke("set_decoder_caps", { decoderCaps: caps }).catch((e) =>
    console.warn("[caps] failed to ship decoder caps to Rust:", e)
  );
});
```

If `App.tsx` has a `useEffect(..., [])` boot block, put it there instead.

- [ ] **Step 3: Verify TypeScript compiles**

```bash
# from tauri-client/
npm run build
```
Expected: passes.

- [ ] **Step 4: Smoke-run the app**

```bash
npm run tauri dev
```

Open DevTools, check console for `[caps]` warnings. Run in DevTools console:
```javascript
JSON.parse(localStorage.getItem("decibell.decoder_caps.v1"))
```
Expected: an array with at least an H.264 entry.

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src/utils/decoderProbe.ts tauri-client/src/main.tsx
git commit -m "feat(caps): WebCodecs decoder probe + ship to Rust at boot"
```

---

## Task 12: Build ClientCapabilities for outgoing protobuf

**Files:**
- Modify: `tauri-client/src-tauri/src/commands/voice.rs`
- Modify: `tauri-client/src-tauri/src/media/caps.rs`

- [ ] **Step 1: Add a helper that builds the proto ClientCapabilities**

In `tauri-client/src-tauri/src/media/caps.rs`, append:

```rust
/// Build the proto ClientCapabilities message from current encoder probe
/// (filtered by user toggles) + decoder caps held in state.
pub fn build_client_capabilities(
    encoder_caps: &[CodecCap],
    decoder_caps: &[CodecCap],
) -> crate::proto::ClientCapabilities {
    crate::proto::ClientCapabilities {
        encode: encoder_caps.iter().map(cap_to_proto).collect(),
        decode: decoder_caps.iter().map(cap_to_proto).collect(),
    }
}

fn cap_to_proto(c: &CodecCap) -> crate::proto::CodecCapability {
    crate::proto::CodecCapability {
        codec: c.codec as i32,
        max_width: c.max_width,
        max_height: c.max_height,
        max_fps: c.max_fps,
    }
}
```

(Adjust `crate::proto::` to whatever module path the prost-generated code is exposed at — search for `pub mod chatproj` or `pub use`.)

- [ ] **Step 2: Attach capabilities to JoinVoiceRequest**

In `tauri-client/src-tauri/src/commands/voice.rs`, find where `JoinVoiceRequest` is constructed (`JoinVoiceRequest { channel_id: ..., }` or similar) and update it:

```rust
use crate::media::caps;

// ... where JoinVoiceRequest is built:
let encoder_caps = caps::get_or_probe_encoders();
// Apply toggles
let use_av1 = *state.use_av1.read().unwrap();
let use_h265 = *state.use_h265.read().unwrap();
let filtered_encode: Vec<_> = encoder_caps
    .into_iter()
    .filter(|c| match c.codec {
        caps::CodecKind::Av1 => use_av1,
        caps::CodecKind::H265 => use_h265,
        _ => true,
    })
    .collect();
let decoder_caps = state.decoder_caps.read().map(|g| g.clone()).unwrap_or_default();
let capabilities = Some(caps::build_client_capabilities(&filtered_encode, &decoder_caps));

let req = JoinVoiceRequest {
    channel_id,
    capabilities,
};
```

- [ ] **Step 3: Verify compiles**

```bash
cargo check
```
Expected: compiles.

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/commands/voice.rs tauri-client/src-tauri/src/media/caps.rs
git commit -m "feat(voice): attach ClientCapabilities to JoinVoiceRequest"
```

---

## Task 13: UpdateCapabilitiesRequest mid-session

**Files:**
- Modify: `tauri-client/src-tauri/src/commands/settings.rs`
- Modify: `tauri-client/src-tauri/src/net/<community connection module>.rs`

- [ ] **Step 1: Find the community connection sender**

Search for where TCP frames are sent to the community server:
```bash
grep -rn "send_frame\|send_packet\|community.*send" tauri-client/src-tauri/src/net/
```

Identify the function that takes a built `Packet` and sends it on the community TLS socket.

- [ ] **Step 2: Add a helper to build + send UpdateCapabilitiesRequest**

In the community connection module (or `commands/settings.rs` if that's where outbound community packets are constructed), add:

```rust
async fn send_update_capabilities_to_active_community(
    state: &AppState,
    new_caps: crate::proto::ClientCapabilities,
) -> Result<(), String> {
    let Some(conn) = state.active_community_connection() else {
        return Ok(()); // Not connected → nothing to update remotely
    };
    let pkt = crate::proto::Packet {
        r#type: crate::proto::packet::Type::UpdateCapabilitiesReq as i32,
        timestamp: chrono::Utc::now().timestamp_millis(),
        auth_token: state.jwt_token().unwrap_or_default(),
        payload: Some(crate::proto::packet::Payload::UpdateCapabilitiesReq(
            crate::proto::UpdateCapabilitiesRequest {
                capabilities: Some(new_caps),
            }
        )),
    };
    conn.send_packet(pkt).await.map_err(|e| e.to_string())
}
```

(Adjust function names like `active_community_connection`, `send_packet` to match the project's actual API.)

- [ ] **Step 3: Trigger send from refresh_caps and set_codec_settings**

Update `refresh_caps` and `set_codec_settings` in `commands/settings.rs` to send the new caps after updating local state:

```rust
#[tauri::command]
pub async fn refresh_caps(state: State<'_, AppState>) -> Result<CapsResponse, String> {
    caps::refresh_encoders();
    let resp = get_caps(state.clone());
    let proto_caps = caps::build_client_capabilities(&resp.encode, &resp.decode);
    let _ = send_update_capabilities_to_active_community(&state, proto_caps).await;
    Ok(resp)
}
```

(Mark the function `async` and `Result`-returning if it isn't already; React's `invoke()` handles both.)

Same treatment for `set_codec_settings`.

- [ ] **Step 4: Verify compiles**

```bash
cargo check
```

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src-tauri/src/commands/settings.rs tauri-client/src-tauri/src/net/
git commit -m "feat(caps): send UpdateCapabilitiesRequest when caps change mid-session"
```

---

## Task 14: Server stores ClientCapabilities on session

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Add a capabilities field to the session struct**

In `src/community/main.cpp`, find the session struct (search for `class Session` or `struct ClientSession`). Add:

```cpp
private:
    chatproj::ClientCapabilities capabilities_;
    std::mutex capabilities_mutex_;

public:
    void set_capabilities(const chatproj::ClientCapabilities& caps) {
        std::lock_guard<std::mutex> lock(capabilities_mutex_);
        capabilities_ = caps;
    }
    chatproj::ClientCapabilities get_capabilities() const {
        std::lock_guard<std::mutex> lock(const_cast<std::mutex&>(capabilities_mutex_));
        return capabilities_;
    }
```

- [ ] **Step 2: Read capabilities from JoinVoiceRequest**

Find the `JOIN_VOICE_REQ` handler. Before/after the existing channel-join logic:

```cpp
if (packet.has_join_voice_req()) {
    const auto& req = packet.join_voice_req();
    if (req.has_capabilities()) {
        session->set_capabilities(req.capabilities());
    }
    // ... existing join logic
}
```

- [ ] **Step 3: Handle UpdateCapabilitiesRequest**

Add a handler near the other packet-type dispatches:

```cpp
} else if (packet.type() == chatproj::Packet::UPDATE_CAPABILITIES_REQ) {
    if (packet.has_update_capabilities_req() &&
        packet.update_capabilities_req().has_capabilities()) {
        session->set_capabilities(packet.update_capabilities_req().capabilities());
        // Rebroadcast presence so other channel members see the new caps.
        std::string channel = session->get_current_voice_channel();
        if (!channel.empty()) {
            manager_.broadcast_voice_presence(channel);
        }
    }
}
```

- [ ] **Step 4: Build the C++ server**

```bash
# from build-servers/
cmake --build . --target chatproj_community
```
Expected: builds. (The capabilities are stored but not yet broadcast — Task 15.)

- [ ] **Step 5: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community): store ClientCapabilities on session, handle UpdateCapabilitiesRequest"
```

---

## Task 15: Server populates VoicePresenceUpdate.user_capabilities

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Find the broadcast_voice_presence function**

Search:
```bash
grep -n "broadcast_voice_presence" src/community/main.cpp
```

This is where `VoicePresenceUpdate` is built and sent.

- [ ] **Step 2: Populate user_capabilities parallel to active_users/user_states**

In `broadcast_voice_presence`, after the existing loop that fills `active_users` and `user_states`, ensure the same iteration order also fills `user_capabilities`. Pseudo-code:

```cpp
for (auto& member_session : voice_channel_members) {
    update->add_active_users(member_session->get_username());
    auto* state = update->add_user_states();
    state->set_username(member_session->get_username());
    state->set_is_muted(member_session->is_muted());
    state->set_is_deafened(member_session->is_deafened());
    *update->add_user_capabilities() = member_session->get_capabilities();
}
```

(Adapt to the existing iteration shape — the key invariant is that `user_capabilities[i]` corresponds to `active_users[i]`.)

- [ ] **Step 3: Build and run the server**

```bash
cmake --build . --target chatproj_community
./chatproj_community
```

- [ ] **Step 4: Manual smoke test with two clients**

Start two Tauri client instances (different accounts), have both join the same voice channel. In one client's DevTools, log the most recent `VoicePresenceUpdate` payload (look in `voiceStore` or wherever it lands — Task 18 will add the cache; for now you can patch a `console.log` into the existing handler temporarily to verify).

Expected: `user_capabilities` array present, length matches `active_users`, each entry has `encode` + `decode` lists.

- [ ] **Step 5: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community): broadcast user_capabilities in VoicePresenceUpdate"
```

---

## Task 16: Server stores chosen_codec/enforced_codec on streams

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Add codec fields to the per-stream registry entry**

Find the data structure used for `active_streams_[channel_id][username]` (likely a `struct StreamEntry` or `VideoStreamInfo`-shaped struct). Add:

```cpp
chatproj::VideoCodec current_codec = chatproj::CODEC_H264_HW;
chatproj::VideoCodec enforced_codec = chatproj::CODEC_UNKNOWN;
uint32_t fps = 0;
uint32_t width = 0;
uint32_t height = 0;
```

(If the existing struct already has fps/width/height, skip those.)

- [ ] **Step 2: Read codec fields from StartStreamRequest**

Find the `START_STREAM_REQ` handler. Update the stream-registration code:

```cpp
if (packet.has_start_stream_req()) {
    const auto& req = packet.start_stream_req();
    auto& entry = active_streams_[req.channel_id()][session->get_username()];
    entry.fps = req.target_fps();
    entry.width = req.resolution_width();
    entry.height = req.resolution_height();
    entry.current_codec = req.chosen_codec() == chatproj::CODEC_UNKNOWN
        ? chatproj::CODEC_H264_HW   // legacy clients pre-negotiation
        : req.chosen_codec();
    entry.enforced_codec = req.enforced_codec();
    // ... existing presence broadcast
}
```

- [ ] **Step 3: Populate the codec fields when broadcasting StreamPresenceUpdate**

Find the `broadcast_stream_presence` (or equivalent) function. Add to the `VideoStreamInfo` build:

```cpp
auto* info = update->add_active_streams();
info->set_stream_id(...);
info->set_owner_username(...);
info->set_has_audio(...);
info->set_resolution_width(entry.width);
info->set_resolution_height(entry.height);
info->set_fps(entry.fps);
info->set_current_codec(entry.current_codec);
info->set_enforced_codec(entry.enforced_codec);
```

- [ ] **Step 4: Build the server**

```bash
cmake --build . --target chatproj_community
```

- [ ] **Step 5: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community): store and rebroadcast chosen_codec/enforced_codec on streams"
```

---

## Task 17: React voiceStore caches userCapabilities

**Files:**
- Modify: `tauri-client/src/stores/voiceStore.ts`

- [ ] **Step 1: Add userCapabilities to the store state**

In `tauri-client/src/stores/voiceStore.ts`, add to the store state interface:

```typescript
import type { ClientCapabilities } from "../types";

interface VoiceStoreState {
  // ... existing fields
  userCapabilities: Record<string, ClientCapabilities>;
}
```

Initialize:
```typescript
  userCapabilities: {},
```

- [ ] **Step 2: Update setVoicePresence (or equivalent action) to populate userCapabilities**

Find the action that handles incoming `VoicePresenceUpdate` events from Tauri (likely `setVoicePresence` or similar). Update it to read the new `user_capabilities` array and store as a username-keyed map:

```typescript
setVoicePresence: (channelId: string, payload: VoicePresenceUpdatePayload) => set((s) => {
  const userCaps: Record<string, ClientCapabilities> = {};
  payload.activeUsers.forEach((username, i) => {
    if (payload.userCapabilities && payload.userCapabilities[i]) {
      userCaps[username] = payload.userCapabilities[i];
    }
  });
  return {
    // ... existing state updates
    userCapabilities: { ...s.userCapabilities, ...userCaps },
  };
}),
```

(Adjust the payload field name to match what the Rust event emitter sends — likely `userCapabilities` in camelCase since the project uses serde with default rename.)

- [ ] **Step 3: Add a selector helper**

Append:

```typescript
export const selectUserCanDecode = (username: string, codec: VideoCodec) =>
  (state: VoiceStoreState): boolean => {
    if (codec === VideoCodec.UNKNOWN) return true;
    const caps = state.userCapabilities[username];
    if (!caps) return false;
    return caps.decode.some((c) => c.codec === codec);
  };
```

- [ ] **Step 4: Verify TypeScript compiles**

```bash
npm run build
```

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src/stores/voiceStore.ts
git commit -m "feat(voice): cache userCapabilities from VoicePresenceUpdate"
```

---

## Task 18: codecSettingsStore for the toggle UI

**Files:**
- Create: `tauri-client/src/stores/codecSettingsStore.ts`

- [ ] **Step 1: Write the store**

Create `tauri-client/src/stores/codecSettingsStore.ts`:

```typescript
import { create } from "zustand";
import { invoke } from "@tauri-apps/api/core";
import type { CodecCapability } from "../types";

interface CodecSettingsState {
  useAv1: boolean;
  useH265: boolean;
  encodeCaps: CodecCapability[];   // probed encoders, post-toggle filtering
  decodeCaps: CodecCapability[];   // probed decoders
  loaded: boolean;

  load: () => Promise<void>;
  setUseAv1: (v: boolean) => Promise<void>;
  setUseH265: (v: boolean) => Promise<void>;
  refresh: () => Promise<void>;
}

export const useCodecSettingsStore = create<CodecSettingsState>((set, get) => ({
  useAv1: true,
  useH265: true,
  encodeCaps: [],
  decodeCaps: [],
  loaded: false,

  load: async () => {
    const settings = await invoke<{ useAv1: boolean; useH265: boolean }>("get_codec_settings");
    const caps = await invoke<{ encode: CodecCapability[]; decode: CodecCapability[] }>("get_caps");
    set({
      useAv1: settings.useAv1,
      useH265: settings.useH265,
      encodeCaps: caps.encode,
      decodeCaps: caps.decode,
      loaded: true,
    });
  },

  setUseAv1: async (v: boolean) => {
    set({ useAv1: v });
    await invoke("set_codec_settings", {
      settings: { useAv1: v, useH265: get().useH265 },
    });
    await get().load(); // re-fetch to update encodeCaps with the new filter
  },

  setUseH265: async (v: boolean) => {
    set({ useH265: v });
    await invoke("set_codec_settings", {
      settings: { useAv1: get().useAv1, useH265: v },
    });
    await get().load();
  },

  refresh: async () => {
    const caps = await invoke<{ encode: CodecCapability[]; decode: CodecCapability[] }>("refresh_caps");
    set({ encodeCaps: caps.encode, decodeCaps: caps.decode });
  },
}));
```

- [ ] **Step 2: Verify TypeScript compiles**

```bash
npm run build
```

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src/stores/codecSettingsStore.ts
git commit -m "feat(stores): codecSettingsStore for toggle UI"
```

---

## Task 19: Settings → Codecs panel UI

**Files:**
- Create: `tauri-client/src/features/settings/CodecsPanel.tsx`
- Modify: settings modal navigation file (find via `ls tauri-client/src/features/settings/`)

- [ ] **Step 1: Read existing settings panel pattern**

```bash
ls tauri-client/src/features/settings/
```

Open one existing panel to see the styling/component conventions (probably plain CSS with the project's existing classnames or Tailwind utility classes).

- [ ] **Step 2: Write CodecsPanel.tsx**

Create `tauri-client/src/features/settings/CodecsPanel.tsx`:

```typescript
import { useEffect } from "react";
import { useCodecSettingsStore } from "../../stores/codecSettingsStore";
import { VideoCodec, type CodecCapability } from "../../types";

const codecLabel = (c: VideoCodec): string => {
  switch (c) {
    case VideoCodec.AV1:    return "AV1";
    case VideoCodec.H265:   return "H.265 / HEVC";
    case VideoCodec.H264_HW: return "H.264 (hardware)";
    case VideoCodec.H264_SW: return "H.264 (software)";
    default:                 return "Unknown";
  }
};

const formatCap = (c: CodecCapability) =>
  `${codecLabel(c.codec)} — up to ${c.maxWidth}×${c.maxHeight} @ ${c.maxFps}fps`;

export function CodecsPanel() {
  const {
    useAv1, useH265, encodeCaps, decodeCaps, loaded,
    load, setUseAv1, setUseH265, refresh,
  } = useCodecSettingsStore();

  useEffect(() => { load(); }, [load]);

  if (!loaded) return <div>Loading…</div>;

  const hasAv1Encode = encodeCaps.some((c) => c.codec === VideoCodec.AV1);
  const hasH265Encode = encodeCaps.some((c) => c.codec === VideoCodec.H265);

  return (
    <div className="settings-panel codecs-panel">
      <h2>Codecs</h2>

      <div className="setting-row">
        <label>
          <input
            type="checkbox"
            checked={useAv1 && hasAv1Encode}
            disabled={!hasAv1Encode}
            onChange={(e) => setUseAv1(e.target.checked)}
          />
          Use AV1 codec when available
        </label>
        {!hasAv1Encode && (
          <p className="hint muted">Your hardware does not support AV1 encoding.</p>
        )}
      </div>

      <div className="setting-row">
        <label>
          <input
            type="checkbox"
            checked={useH265 && hasH265Encode}
            disabled={!hasH265Encode}
            onChange={(e) => setUseH265(e.target.checked)}
          />
          Use H.265 / HEVC codec when available
        </label>
        {!hasH265Encode && (
          <p className="hint muted">Your hardware does not support H.265 encoding.</p>
        )}
      </div>

      <button className="refresh-button" onClick={refresh}>
        Refresh codec capabilities
      </button>

      <section className="probe-summary">
        <h3>Detected encoders</h3>
        {encodeCaps.length === 0
          ? <p className="muted">None detected.</p>
          : <ul>{encodeCaps.map((c) => <li key={c.codec}>{formatCap(c)}</li>)}</ul>}

        <h3>Detected decoders</h3>
        {decodeCaps.length === 0
          ? <p className="muted">None detected.</p>
          : <ul>{decodeCaps.map((c) => <li key={c.codec}>{formatCap(c)}</li>)}</ul>}
      </section>
    </div>
  );
}
```

- [ ] **Step 3: Wire it into the settings modal navigation**

Find where the existing settings panels (Account, Notifications, etc.) are listed. Add a new entry pointing to `CodecsPanel`. The exact integration shape depends on the existing pattern — follow it.

- [ ] **Step 4: Verify TypeScript compiles**

```bash
npm run build
```

- [ ] **Step 5: Smoke-run the app**

```bash
npm run tauri dev
```

Open Settings → Codecs. Verify:
- Both toggles show, possibly disabled with explanation if hardware lacks the encoder.
- Encoder/decoder lists populate after a moment.
- Toggling a switch and reopening the panel keeps the setting (persisted via Task 9's config save).
- "Refresh codec capabilities" button re-runs the probe.

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src/features/settings/CodecsPanel.tsx tauri-client/src/features/settings/
git commit -m "feat(settings): Codecs panel with toggles, probe summary, refresh button"
```

---

## Task 20: End-to-end smoke test for capability flow

- [ ] **Step 1: Start the community server**

```bash
# from build-servers/
./chatproj_community
```

- [ ] **Step 2: Start two Tauri clients with different accounts**

```bash
# in tauri-client/
npm run tauri dev
# (open a second instance via a separate shell + a second user account)
```

- [ ] **Step 3: Both join the same voice channel**

- [ ] **Step 4: In each client's DevTools, inspect the voiceStore**

```javascript
useVoiceStore.getState().userCapabilities
```

Expected: an object keyed by username, with each entry containing `encode` and `decode` arrays.

- [ ] **Step 5: Toggle AV1 off in one client**

In Settings → Codecs, uncheck AV1 (if available). Switch back to the voice channel. The OTHER client's voiceStore should now reflect the updated capabilities (no AV1 in this user's encode list) — driven by Task 13's `UpdateCapabilitiesRequest` round-trip.

- [ ] **Step 6: Verify caps.json on disk**

Look in the OS config dir (e.g. `%APPDATA%\decibell\encoder_caps.json` on Windows). Should contain a JSON array of CodecCap entries.

- [ ] **Step 7: Restart one client**

The cached caps should load instantly without a re-probe (no encoder probe spam in the logs). Click "Refresh codec capabilities" → see the probe re-run.

- [ ] **Step 8: Commit any small fixes found during smoke test**

```bash
git add -u
git commit -m "fix: smoke-test fixes from Plan A integration"
```

---

## Task 21: Final verification checklist

- [ ] Plan A spec coverage check — confirm each item is implemented:
  - [ ] §3.1 wire format (`VideoCodec`, `CodecCapability`, `ClientCapabilities`) — Tasks 1, 5
  - [ ] §3.2 policy ceilings — Tasks 7, 11
  - [ ] §3.3 probing once on first launch + cache + manual refresh + toggles + H.264 fallback — Tasks 7, 10, 11, 18, 19
  - [ ] §4.1 capabilities on JoinVoiceRequest — Tasks 2, 12, 14
  - [ ] §4.2 user_capabilities on VoicePresenceUpdate — Tasks 2, 15, 17
  - [ ] §4.6 UpdateCapabilitiesRequest mid-session via "Refresh" → broadcast — Tasks 3, 13, 14
  - [ ] §7.1 Settings → Codecs panel — Tasks 18, 19
  - [ ] §9 wire-protocol checklist — Tasks 1-4, 14-16
  - [ ] H.264_HW = 1 wire value preserved — Task 4

- [ ] No new behavior change visible to the streaming pipeline — H.264 still hardcoded as the only codec produced. Plan B and Plan C will use the plumbing.
