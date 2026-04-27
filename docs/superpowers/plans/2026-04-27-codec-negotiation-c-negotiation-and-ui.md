# Codec Negotiation — Plan C: Auto-Negotiation, Swap Mechanics, and UI

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Depends on:** Plan A (capability plumbing) and Plan B (encoder backends) — both must be merged first.

**Goal:** Wire up the LCD codec picker, the 200ms downgrade debounce, the 30s upgrade cooldown, the encoder swap mechanics, the React per-packet decoder reconfiguration, the codec-aware bitrate preset table, and all the UI surface (enforce dropdown, watch-button gating, codec/lock badge, toasts) to ship the full negotiation feature as designed.

**Architecture:** A new `codec_selection.rs` module holds the pure LCD picker function (thoroughly tested) and a `CodecSelector` struct that owns the timer state. The `video_pipeline.rs` integrates the selector, listening to a small new server→streamer notification stream (`StreamWatcherNotify`) for watcher join/leave events. Encoder swaps happen out-of-band: build the new encoder while the old one keeps producing frames, force a keyframe, atomically swap, tear down the old encoder asynchronously. The React `StreamVideoPlayer` watches `currentCodec` from `StreamPresenceUpdate` events and the per-packet codec byte from incoming UDP packets — when either changes, it tears down the old `VideoDecoder` and configures a new one. UI components add the enforce dropdown to `StreamConfigDialog`, the watch-button gating + tooltip to the stream presence list, the codec/lock badge to the player tile, and toast notifications driven by `StreamCodecChangedNotify.reason`.

**Tech Stack:** Tokio timers (`tokio::time::sleep` with cancellation via `Notify`), Tauri events for streamer↔Rust ↔React communication, existing `toastStore` from Plan A's discovery (`tauri-client/src/stores/toastStore.ts` already exists).

**Spec reference:** `docs/superpowers/specs/2026-04-27-video-codec-negotiation-design.md` §§ 5, 6, 7.2, 7.3, 7.4, 7.5, 8.

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `proto/messages.proto` | Modify | New packet type `STREAM_WATCHER_NOTIFY = 60` and `StreamWatcherNotify` message |
| `tauri-client/src-tauri/src/media/codec_selection.rs` | Create | LCD picker (pure fn) + `CodecSelector` (timer state + swap orchestration) |
| `tauri-client/src-tauri/src/media/bitrate_preset.rs` | Create | Codec-aware bitrate calculation from quality preset |
| `tauri-client/src-tauri/src/media/mod.rs` | Modify | Add new modules |
| `tauri-client/src-tauri/src/media/video_pipeline.rs` | Modify | Integrate `CodecSelector`, perform encoder swap, send `StreamCodecChangedNotify` |
| `tauri-client/src-tauri/src/commands/streaming.rs` | Modify | Replace `force_codec` dev shim with production `enforced_codec`; pass through to pipeline |
| `tauri-client/src-tauri/src/net/<community connection>.rs` | Modify | Subscribe to `StreamWatcherNotify` from server; forward to active video pipeline |
| `src/community/main.cpp` | Modify | Send `StreamWatcherNotify` to streamer when `WatchStreamRequest`/`StopWatchingRequest` arrives. Handle `StreamCodecChangedNotify` from streamer (already scaffolded in Plan A — wire it up). Defensive `WatchStreamRequest` rejection when `enforced_codec` doesn't match watcher caps. |
| `tauri-client/src/features/voice/StreamVideoPlayer.tsx` | Modify | Reconfigure decoder when per-packet codec byte changes; render badge + lock icon |
| `tauri-client/src/features/voice/StreamConfigDialog.tsx` (or `CaptureSourcePicker.tsx`) | Modify | Add "Codec" dropdown (`Auto` + per-encodable-codec entries) |
| `tauri-client/src/features/voice/StreamViewPanel.tsx` (or wherever the watch-list lives) | Modify | Compute `canDecode` per stream; gray out/disable Watch button with tooltip |
| `tauri-client/src/features/voice/CodecBadge.tsx` | Create | Pill badge component with codec/res/fps + optional lock icon |
| `tauri-client/src/utils/codecToasts.ts` | Create | Toast text builder from `StreamCodecChangedNotify.reason` |
| `tauri-client/src/types/index.ts` | Modify | Add `StreamCodecChangedNotify`, `StreamWatcherNotify` payload types |

---

## Task 1: Add StreamWatcherNotify packet type to proto

**Files:**
- Modify: `proto/messages.proto`

The streamer needs to know when a watcher starts or stops watching their stream. Plan A's signaling work didn't include this; add it now.

- [ ] **Step 1: Add packet type and oneof entry**

In `proto/messages.proto`, add to `Packet.Type` after `UPDATE_CAPABILITIES_REQ = 59;`:

```protobuf
    STREAM_WATCHER_NOTIFY = 60;
```

In the `Packet.payload` oneof, add after `update_capabilities_req = 61;`:

```protobuf
    StreamWatcherNotify stream_watcher_notify = 62;
```

- [ ] **Step 2: Add the message definition**

Append to `proto/messages.proto`:

```protobuf
// Sent by the community server to the streamer when a watcher joins or
// leaves their stream. The streamer uses this to drive the LCD picker
// and decide whether to renegotiate the codec.
message StreamWatcherNotify {
  string channel_id = 1;
  string streamer_username = 2;
  string watcher_username = 3;
  enum Action {
    ACTION_UNKNOWN = 0;
    JOINED = 1;
    LEFT = 2;
  }
  Action action = 4;
}
```

- [ ] **Step 3: Verify Rust + C++ regen compiles**

```bash
# from tauri-client/src-tauri/
cargo check
# from build-servers/
cmake --build . --target chatproj_common
```
Both expected to compile.

- [ ] **Step 4: Add the TypeScript type**

In `tauri-client/src/types/index.ts`, append:

```typescript
export const StreamWatcherAction = {
  UNKNOWN: 0,
  JOINED: 1,
  LEFT: 2,
} as const;
export type StreamWatcherAction = (typeof StreamWatcherAction)[keyof typeof StreamWatcherAction];

export interface StreamWatcherNotify {
  channelId: string;
  streamerUsername: string;
  watcherUsername: string;
  action: StreamWatcherAction;
}

export const StreamCodecChangeReason = {
  UNKNOWN: 0,
  WATCHER_JOINED_LOW_CAPS: 1,
  LIMITING_WATCHER_LEFT: 2,
  STREAMER_INITIATED: 3,
} as const;
export type StreamCodecChangeReason = (typeof StreamCodecChangeReason)[keyof typeof StreamCodecChangeReason];

export interface StreamCodecChangedNotify {
  channelId: string;
  streamerUsername: string;
  newCodec: VideoCodec;
  newWidth: number;
  newHeight: number;
  newFps: number;
  reason: StreamCodecChangeReason;
}
```

- [ ] **Step 5: Commit**

```bash
git add proto/messages.proto tauri-client/src/types/index.ts
git commit -m "proto: add STREAM_WATCHER_NOTIFY for streamer-side LCD evaluation"
```

---

## Task 2: Server sends StreamWatcherNotify on watch events

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Find the WATCH_STREAM_REQ and STOP_WATCHING_REQ handlers**

```bash
grep -n "WATCH_STREAM_REQ\|STOP_WATCHING_REQ" src/community/main.cpp
```

- [ ] **Step 2: Add a helper to send StreamWatcherNotify to the streamer**

Find the `SessionManager` class. Add a helper method:

```cpp
void SessionManager::notify_streamer_of_watcher(
    const std::string& channel_id,
    const std::string& streamer_username,
    const std::string& watcher_username,
    chatproj::StreamWatcherNotify::Action action) {

    std::shared_ptr<Session> streamer_session;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        for (auto& [token, sess] : sessions_) {
            if (sess->get_username() == streamer_username) {
                streamer_session = sess;
                break;
            }
        }
    }
    if (!streamer_session) return;

    chatproj::Packet pkt;
    pkt.set_type(chatproj::Packet::STREAM_WATCHER_NOTIFY);
    pkt.set_timestamp(std::chrono::duration_cast<std::chrono::milliseconds>(
        std::chrono::system_clock::now().time_since_epoch()).count());
    auto* notify = pkt.mutable_stream_watcher_notify();
    notify->set_channel_id(channel_id);
    notify->set_streamer_username(streamer_username);
    notify->set_watcher_username(watcher_username);
    notify->set_action(action);
    streamer_session->send_packet(pkt);
}
```

(Adapt `send_packet` to the existing send-to-session helper used elsewhere in the file.)

- [ ] **Step 3: Call the helper from watch handlers**

In the `WATCH_STREAM_REQ` handler, after the existing watcher-add logic:

```cpp
const auto& req = packet.watch_stream_req();
manager_.notify_streamer_of_watcher(
    req.channel_id(),
    req.target_username(),
    session->get_username(),
    chatproj::StreamWatcherNotify::JOINED);
```

In the `STOP_WATCHING_REQ` handler, after the existing watcher-remove logic:

```cpp
const auto& req = packet.stop_watching_req();
manager_.notify_streamer_of_watcher(
    req.channel_id(),
    req.target_username(),
    session->get_username(),
    chatproj::StreamWatcherNotify::LEFT);
```

Also handle the implicit "watcher left voice channel" case — find where voice-channel-leave logic runs (search for `LEAVE_VOICE_REQ` and the disconnect cleanup). For each stream the leaving user was watching, send a LEFT notify to that streamer. (You can iterate `stream_watchers_[channel_id]` to find streams the leaving user was a watcher of.)

- [ ] **Step 4: Build the server**

```bash
cmake --build . --target chatproj_community
```

- [ ] **Step 5: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community): send StreamWatcherNotify on watch/unwatch/disconnect"
```

---

## Task 3: Streamer receives StreamWatcherNotify

**Files:**
- Modify: `tauri-client/src-tauri/src/net/<community connection>.rs`

- [ ] **Step 1: Add a Tauri channel for streamer-side watcher events**

In the community connection module, in the packet dispatch loop, add a case for `STREAM_WATCHER_NOTIFY`:

```rust
chatproj::packet::Type::StreamWatcherNotify => {
    if let Some(crate::proto::packet::Payload::StreamWatcherNotify(notify)) = pkt.payload {
        // Forward to whatever channel the active video pipeline subscribes to.
        // Use a broadcast channel held in AppState (see Task 4).
        let _ = state.watcher_event_tx().send(WatcherEvent {
            channel_id: notify.channel_id,
            streamer_username: notify.streamer_username,
            watcher_username: notify.watcher_username,
            action: notify.action,
        });
    }
}
```

- [ ] **Step 2: Define WatcherEvent and the broadcast channel**

In `tauri-client/src-tauri/src/state.rs`, add:

```rust
use tokio::sync::broadcast;

#[derive(Clone, Debug)]
pub struct WatcherEvent {
    pub channel_id: String,
    pub streamer_username: String,
    pub watcher_username: String,
    pub action: i32, // proto enum
}

// inside AppState
    pub watcher_event_tx: broadcast::Sender<WatcherEvent>,

// in AppState::new (or default)
        watcher_event_tx: broadcast::channel(64).0,
```

Add a getter:

```rust
impl AppState {
    pub fn watcher_event_tx(&self) -> broadcast::Sender<WatcherEvent> {
        self.watcher_event_tx.clone()
    }
}
```

- [ ] **Step 3: Verify compiles**

```bash
cargo check
```

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/state.rs tauri-client/src-tauri/src/net/
git commit -m "feat(net): receive StreamWatcherNotify, expose watcher event broadcast"
```

---

## Task 4: Bitrate preset module (codec-aware, with tests)

**Files:**
- Create: `tauri-client/src-tauri/src/media/bitrate_preset.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

This is a pure function — perfect for TDD.

- [ ] **Step 1: Add module declaration**

In `tauri-client/src-tauri/src/media/mod.rs`:

```rust
pub mod bitrate_preset;
```

- [ ] **Step 2: Write failing tests**

Create `tauri-client/src-tauri/src/media/bitrate_preset.rs`:

```rust
//! Codec-aware bitrate preset table.
//!
//! Spec §8: bitrate = bpp_s × width × height × fps,
//! where bpp_s is the bits-per-pixel-per-second multiplier.

use crate::media::caps::CodecKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Low,
    Medium,
    High,
    Custom(u32), // explicit bitrate kbps; bypasses table
}

/// Returns kbps for the given quality + codec + resolution + fps.
/// For Custom, returns the explicit bitrate regardless of codec.
pub fn bitrate_kbps(quality: Quality, codec: CodecKind, width: u32, height: u32, fps: u32) -> u32 {
    if let Quality::Custom(kbps) = quality {
        return kbps;
    }
    let bpp_s = bpp_s_for(quality, codec);
    let total_bits_per_sec = bpp_s * (width as f64) * (height as f64) * (fps as f64);
    let kbps = (total_bits_per_sec / 1000.0).round() as u32;
    kbps.clamp(300, 50_000)  // sane absolute floor + ceiling
}

fn bpp_s_for(quality: Quality, codec: CodecKind) -> f64 {
    let row = match codec {
        CodecKind::H264Hw | CodecKind::H264Sw => (0.020, 0.050, 0.080),
        CodecKind::H265 => (0.013, 0.033, 0.054),
        CodecKind::Av1 => (0.010, 0.025, 0.040),
        CodecKind::Unknown => (0.020, 0.050, 0.080), // fall back to H.264 multipliers
    };
    match quality {
        Quality::Low => row.0,
        Quality::Medium => row.1,
        Quality::High => row.2,
        Quality::Custom(_) => unreachable!("handled above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_1080p60_medium_is_about_6mbps() {
        let kbps = bitrate_kbps(Quality::Medium, CodecKind::H264Hw, 1920, 1080, 60);
        // 1920*1080*60 = 124.4M; * 0.050 = 6.22 Mbps
        assert!((6000..=6300).contains(&kbps), "got {} kbps", kbps);
    }

    #[test]
    fn av1_4k60_high_is_about_20mbps() {
        let kbps = bitrate_kbps(Quality::High, CodecKind::Av1, 3840, 2160, 60);
        // 3840*2160*60 = 497.6M; * 0.040 = 19.9 Mbps
        assert!((19_500..=20_500).contains(&kbps), "got {} kbps", kbps);
    }

    #[test]
    fn h265_1440p60_high_is_about_12mbps() {
        let kbps = bitrate_kbps(Quality::High, CodecKind::H265, 2560, 1440, 60);
        // 2560*1440*60 = 221M; * 0.054 = 11.94 Mbps
        assert!((11_500..=12_500).contains(&kbps), "got {} kbps", kbps);
    }

    #[test]
    fn custom_bypasses_table() {
        let kbps = bitrate_kbps(Quality::Custom(8500), CodecKind::Av1, 3840, 2160, 60);
        assert_eq!(kbps, 8500);
    }

    #[test]
    fn av1_uses_lower_bitrate_than_h264_for_same_quality() {
        let h264 = bitrate_kbps(Quality::Medium, CodecKind::H264Hw, 1920, 1080, 60);
        let av1 = bitrate_kbps(Quality::Medium, CodecKind::Av1, 1920, 1080, 60);
        assert!(av1 < h264, "AV1 {} should be less than H.264 {}", av1, h264);
        // Should be roughly half
        assert!((h264 as f64 / av1 as f64) > 1.5, "ratio is {}", h264 as f64 / av1 as f64);
    }

    #[test]
    fn floor_clamp_kicks_in_at_tiny_resolutions() {
        let kbps = bitrate_kbps(Quality::Low, CodecKind::Av1, 320, 240, 15);
        assert_eq!(kbps, 300, "should clamp to floor");
    }
}
```

- [ ] **Step 3: Run tests — verify they fail (module doesn't compile yet)**

```bash
cargo test --lib bitrate_preset
```
Expected: tests fail to compile because the module is brand new — actually the file IS written, so they should compile. Run and verify all tests pass:

Expected: all 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/bitrate_preset.rs tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(bitrate): codec-aware preset table with tests"
```

---

## Task 5: codec_selection module — LCD picker (pure function with tests)

**Files:**
- Create: `tauri-client/src-tauri/src/media/codec_selection.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

- [ ] **Step 1: Add module declaration**

In `tauri-client/src-tauri/src/media/mod.rs`:

```rust
pub mod codec_selection;
```

- [ ] **Step 2: Write the LCD picker with tests**

Create `tauri-client/src-tauri/src/media/codec_selection.rs`:

```rust
//! Codec selection — the LCD (lowest common denominator) picker.
//!
//! Spec §5.1: given a set of watchers and the streamer's encode caps,
//! pick the highest-priority codec all watchers can decode, with
//! resolution/fps clamped to the smallest of (streamer original,
//! streamer encode ceiling, every watcher's decode ceiling).

use crate::media::caps::{CodecCap, CodecKind};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamSettings {
    pub codec: CodecKind,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// Encoder priority order — first that satisfies all constraints wins.
const PRIORITY: [CodecKind; 4] = [
    CodecKind::Av1,
    CodecKind::H265,
    CodecKind::H264Hw,
    CodecKind::H264Sw,
];

#[derive(Clone, Debug)]
pub struct Toggles {
    pub use_av1: bool,
    pub use_h265: bool,
}

impl Toggles {
    fn allows(&self, codec: CodecKind) -> bool {
        match codec {
            CodecKind::Av1 => self.use_av1,
            CodecKind::H265 => self.use_h265,
            _ => true,
        }
    }
}

fn find_cap(caps: &[CodecCap], codec: CodecKind) -> Option<&CodecCap> {
    caps.iter().find(|c| c.codec == codec)
}

/// Pure LCD picker. `original` is the streamer's preferred ceiling
/// (codec is ignored — this function picks codec; the dimensions/fps cap
/// the result from above).
pub fn pick(
    streamer_encode: &[CodecCap],
    watchers_decode: &[Vec<CodecCap>],
    toggles: &Toggles,
    original: StreamSettings,
) -> StreamSettings {
    for &candidate in &PRIORITY {
        if !toggles.allows(candidate) { continue; }

        let Some(streamer_cap) = find_cap(streamer_encode, candidate) else { continue };

        // Each watcher must have this codec in their decode list.
        let mut watcher_caps_for_candidate: Vec<&CodecCap> = Vec::with_capacity(watchers_decode.len());
        let mut all_have = true;
        for w in watchers_decode {
            match find_cap(w, candidate) {
                Some(c) => watcher_caps_for_candidate.push(c),
                None => { all_have = false; break; }
            }
        }
        if !all_have { continue; }

        // All watchers can decode `candidate`. Compute the dim/fps clamp.
        let mut width = original.width.min(streamer_cap.max_width);
        let mut height = original.height.min(streamer_cap.max_height);
        let mut fps = original.fps.min(streamer_cap.max_fps);
        for c in &watcher_caps_for_candidate {
            width = width.min(c.max_width);
            height = height.min(c.max_height);
            fps = fps.min(c.max_fps);
        }

        return StreamSettings { codec: candidate, width, height, fps };
    }

    // Spec §5.1 step 3: unreachable in practice (H.264 decode is
    // universal). Defensive fallback: pick the highest-priority codec
    // satisfying the most watchers, clamped to the streamer's caps.
    eprintln!("[codec_selection] LCD failed — falling back");
    let fallback_codec = streamer_encode.first().map(|c| c.codec).unwrap_or(CodecKind::H264Hw);
    let fallback_cap = find_cap(streamer_encode, fallback_codec);
    StreamSettings {
        codec: fallback_codec,
        width: original.width.min(fallback_cap.map(|c| c.max_width).unwrap_or(original.width)),
        height: original.height.min(fallback_cap.map(|c| c.max_height).unwrap_or(original.height)),
        fps: original.fps.min(fallback_cap.map(|c| c.max_fps).unwrap_or(original.fps)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(codec: CodecKind, w: u32, h: u32, fps: u32) -> CodecCap {
        CodecCap { codec, max_width: w, max_height: h, max_fps: fps }
    }
    fn toggles_all() -> Toggles {
        Toggles { use_av1: true, use_h265: true }
    }
    fn original_4k60() -> StreamSettings {
        StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 }
    }

    #[test]
    fn no_watchers_returns_streamers_top_codec() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let r = pick(&enc, &[], &toggles_all(), original_4k60());
        assert_eq!(r.codec, CodecKind::Av1);
        assert_eq!(r.width, 3840);
    }

    #[test]
    fn watcher_with_av1_decode_keeps_av1() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let watchers = vec![vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 3840, 2160, 60)]];
        let r = pick(&enc, &watchers, &toggles_all(), original_4k60());
        assert_eq!(r.codec, CodecKind::Av1);
        assert_eq!(r.width, 3840);
    }

    #[test]
    fn h264_only_watcher_forces_h264_and_drops_to_1440p() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let watchers = vec![vec![cap(CodecKind::H264Hw, 3840, 2160, 60)]];
        let r = pick(&enc, &watchers, &toggles_all(), original_4k60());
        assert_eq!(r.codec, CodecKind::H264Hw);
        assert_eq!(r.width, 2560);
        assert_eq!(r.height, 1440);
        assert_eq!(r.fps, 60);
    }

    #[test]
    fn user_disabled_av1_skips_av1_even_if_supported() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H265, 3840, 2160, 60)];
        let watchers = vec![vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H265, 3840, 2160, 60)]];
        let toggles = Toggles { use_av1: false, use_h265: true };
        let r = pick(&enc, &watchers, &toggles, original_4k60());
        assert_eq!(r.codec, CodecKind::H265);
    }

    #[test]
    fn watcher_with_only_h265_decode_picks_h265() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H265, 3840, 2160, 60)];
        let watchers = vec![
            vec![cap(CodecKind::H265, 3840, 2160, 60), cap(CodecKind::H264Hw, 3840, 2160, 60)],
        ];
        let r = pick(&enc, &watchers, &toggles_all(), original_4k60());
        assert_eq!(r.codec, CodecKind::H265);
    }

    #[test]
    fn streamer_no_av1_encode_picks_h265_even_if_watchers_decode_av1() {
        let enc = vec![cap(CodecKind::H265, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let watchers = vec![vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H265, 3840, 2160, 60)]];
        let r = pick(&enc, &watchers, &toggles_all(), original_4k60());
        assert_eq!(r.codec, CodecKind::H265);
    }

    #[test]
    fn original_resolution_is_upper_bound() {
        // Streamer encoded original 1080p; even if everyone could do 4K,
        // we don't EXCEED what was originally requested.
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60)];
        let watchers = vec![vec![cap(CodecKind::Av1, 3840, 2160, 60)]];
        let original = StreamSettings { codec: CodecKind::Av1, width: 1920, height: 1080, fps: 60 };
        let r = pick(&enc, &watchers, &toggles_all(), original);
        assert_eq!(r.width, 1920);
        assert_eq!(r.height, 1080);
    }
}
```

- [ ] **Step 3: Run tests**

```bash
cargo test --lib codec_selection
```
Expected: all 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/codec_selection.rs tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(codec): LCD picker with comprehensive tests"
```

---

## Task 6: CodecSelector — timer state, debounce/cooldown orchestration

**Files:**
- Modify: `tauri-client/src-tauri/src/media/codec_selection.rs`

The selector is the stateful coordinator: it holds the current settings, original settings, the timer state, and exposes:
- `on_watcher_joined(...)` — recompute target, maybe schedule downgrade
- `on_watcher_left(...)` — recompute target, maybe schedule upgrade
- A swap callback that fires when timers complete

- [ ] **Step 1: Add the CodecSelector struct**

Append to `codec_selection.rs`:

```rust
use crate::media::caps::CodecCap;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const DOWNGRADE_DEBOUNCE: Duration = Duration::from_millis(200);
const UPGRADE_COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub enum SwapReason {
    WatcherJoinedLowCaps,
    LimitingWatcherLeft,
    StreamerInitiated,
}

/// Emitted when a swap should happen. The pipeline owns the actual
/// encoder and performs the swap.
#[derive(Clone, Debug)]
pub struct SwapEvent {
    pub target: StreamSettings,
    pub reason: SwapReason,
}

pub struct CodecSelector {
    inner: Arc<Mutex<SelectorState>>,
    swap_tx: mpsc::UnboundedSender<SwapEvent>,
}

struct SelectorState {
    original: StreamSettings,
    current: StreamSettings,
    enforced: Option<CodecKind>,
    streamer_encode: Vec<CodecCap>,
    toggles: Toggles,
    /// Username → that user's decode caps (only watchers, not all voice members).
    watchers: HashMap<String, Vec<CodecCap>>,
    /// Active timer (downgrade or upgrade); generation counter so a stale
    /// fired timer can detect it was superseded.
    timer: Option<TimerInfo>,
    timer_generation: u64,
}

struct TimerInfo {
    handle: JoinHandle<()>,
    kind: TimerKind,
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimerKind { Downgrade, Upgrade }

impl CodecSelector {
    pub fn new(
        initial: StreamSettings,
        enforced: Option<CodecKind>,
        streamer_encode: Vec<CodecCap>,
        toggles: Toggles,
    ) -> (Self, mpsc::UnboundedReceiver<SwapEvent>) {
        let (swap_tx, swap_rx) = mpsc::unbounded_channel();
        let state = SelectorState {
            original: initial,
            current: initial,
            enforced,
            streamer_encode,
            toggles,
            watchers: HashMap::new(),
            timer: None,
            timer_generation: 0,
        };
        (Self { inner: Arc::new(Mutex::new(state)), swap_tx }, swap_rx)
    }

    pub fn current_settings(&self) -> StreamSettings {
        self.inner.lock().unwrap().current
    }

    pub fn enforced(&self) -> Option<CodecKind> {
        self.inner.lock().unwrap().enforced
    }

    /// Reflect a swap that the pipeline actually performed (updates `current`).
    pub fn record_swap(&self, new: StreamSettings) {
        let mut s = self.inner.lock().unwrap();
        s.current = new;
        s.timer = None; // swap done, no active timer
    }

    pub fn on_watcher_joined(&self, username: String, decode: Vec<CodecCap>) {
        let mut s = self.inner.lock().unwrap();
        if s.enforced.is_some() { return; } // §5.5: no auto changes under enforcement
        s.watchers.insert(username, decode);
        self.recompute_locked(&mut s);
    }

    pub fn on_watcher_left(&self, username: &str) {
        let mut s = self.inner.lock().unwrap();
        if s.enforced.is_some() { return; }
        s.watchers.remove(username);
        self.recompute_locked(&mut s);
    }

    fn recompute_locked(&self, s: &mut SelectorState) {
        let watchers_decode: Vec<Vec<CodecCap>> = s.watchers.values().cloned().collect();
        let target = pick(&s.streamer_encode, &watchers_decode, &s.toggles, s.original);

        if target == s.current {
            // Nothing to do. If a previous upgrade was scheduled and is now
            // unnecessary, cancel it.
            if let Some(t) = s.timer.take() {
                t.handle.abort();
            }
            return;
        }

        let new_kind = if priority_index(target.codec) < priority_index(s.current.codec) {
            // Higher-priority codec → upgrade
            TimerKind::Upgrade
        } else if priority_index(target.codec) > priority_index(s.current.codec) {
            TimerKind::Downgrade
        } else {
            // Same codec, dim/fps changed. Treat shrinking dims as downgrade,
            // growing as upgrade.
            if target.width < s.current.width || target.height < s.current.height || target.fps < s.current.fps {
                TimerKind::Downgrade
            } else {
                TimerKind::Upgrade
            }
        };

        // Spec §5.4: cooldown is monotonic — if an existing upgrade timer is
        // running and the new target is also an upgrade (possibly higher),
        // do NOT reset the timer. Just let it fire and pick up the latest
        // target then.
        if let Some(existing) = &s.timer {
            if existing.kind == TimerKind::Upgrade && new_kind == TimerKind::Upgrade {
                return; // monotonic — leave it
            }
            // Existing is downgrade or kind changed — abort and re-arm.
            existing.handle.abort();
        }

        s.timer_generation = s.timer_generation.wrapping_add(1);
        let gen = s.timer_generation;
        let delay = match new_kind {
            TimerKind::Downgrade => DOWNGRADE_DEBOUNCE,
            TimerKind::Upgrade => UPGRADE_COOLDOWN,
        };
        let inner = self.inner.clone();
        let swap_tx = self.swap_tx.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let mut state = inner.lock().unwrap();
            // Verify we are still the active timer
            if state.timer_generation != gen { return; }
            // Recompute target one more time at fire time — watchers may have
            // changed during the wait.
            let watchers_decode: Vec<Vec<CodecCap>> = state.watchers.values().cloned().collect();
            let final_target = pick(&state.streamer_encode, &watchers_decode, &state.toggles, state.original);
            if final_target == state.current { return; }
            let reason = match new_kind {
                TimerKind::Downgrade => SwapReason::WatcherJoinedLowCaps,
                TimerKind::Upgrade => SwapReason::LimitingWatcherLeft,
            };
            // Don't update state.current here — record_swap does that after
            // the pipeline confirms the swap completed.
            state.timer = None;
            drop(state);
            let _ = swap_tx.send(SwapEvent { target: final_target, reason });
        });

        s.timer = Some(TimerInfo { handle, kind: new_kind, generation: gen });
    }
}

fn priority_index(codec: CodecKind) -> usize {
    PRIORITY.iter().position(|c| *c == codec).unwrap_or(usize::MAX)
}
```

- [ ] **Step 2: Add unit tests for the selector**

Add to the existing `tests` mod:

```rust
    use std::time::Instant;

    fn caps(decoders: &[(CodecKind, u32, u32, u32)]) -> Vec<CodecCap> {
        decoders.iter().map(|(c, w, h, f)| cap(*c, *w, *h, *f)).collect()
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn downgrade_on_watcher_join_fires_after_debounce() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let initial = StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 };
        let (selector, mut rx) = CodecSelector::new(initial, None, enc, toggles_all());

        // Join: H.264-only watcher
        selector.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        // Should NOT have fired yet (debounce 200ms)
        assert!(rx.try_recv().is_err());

        tokio::time::advance(Duration::from_millis(250)).await;

        let event = rx.recv().await.expect("swap event");
        assert_eq!(event.target.codec, CodecKind::H264Hw);
        assert_eq!(event.target.width, 2560);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn rapid_joins_coalesce_into_one_swap() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let initial = StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 };
        let (selector, mut rx) = CodecSelector::new(initial, None, enc, toggles_all());

        selector.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_millis(100)).await;
        selector.on_watcher_joined("bob".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_millis(250)).await;

        // First event should be the coalesced final target
        let event = rx.recv().await.expect("swap event");
        assert_eq!(event.target.codec, CodecKind::H264Hw);
        // Should not get a second event
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn upgrade_after_limiting_watcher_leaves_waits_30s() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let initial = StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 };
        let (selector, mut rx) = CodecSelector::new(initial, None, enc, toggles_all());

        selector.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_millis(250)).await;
        let downgrade = rx.recv().await.unwrap();
        selector.record_swap(downgrade.target); // simulate pipeline completing the swap

        selector.on_watcher_left("alice");
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(rx.try_recv().is_err(), "shouldn't fire before 30s");

        tokio::time::advance(Duration::from_secs(25)).await;
        let upgrade = rx.recv().await.expect("upgrade event");
        assert_eq!(upgrade.target.codec, CodecKind::Av1);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn upgrade_cancelled_if_low_cap_watcher_rejoins_during_cooldown() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let initial = StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 };
        let (selector, mut rx) = CodecSelector::new(initial, None, enc, toggles_all());

        selector.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_millis(250)).await;
        let downgrade = rx.recv().await.unwrap();
        selector.record_swap(downgrade.target);

        selector.on_watcher_left("alice");
        tokio::time::advance(Duration::from_secs(10)).await;
        // alice rejoins during cooldown
        selector.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_secs(60)).await;
        // No upgrade fired, no further downgrade fired (already at H.264)
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn enforcement_blocks_all_recompute() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let initial = StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 };
        let (selector, mut rx) = CodecSelector::new(initial, Some(CodecKind::Av1), enc, toggles_all());

        selector.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(rx.try_recv().is_err(), "enforcement should block all recompute");
    }
```

- [ ] **Step 3: Run tests**

```bash
cargo test --lib codec_selection
```
Expected: all tests pass (including the original 7 from Task 5 plus the 5 new tokio::test tests). If any fail, fix the selector logic until they pass.

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/codec_selection.rs
git commit -m "feat(codec): CodecSelector with debounce/cooldown timers and tests"
```

---

## Task 7: Encoder swap mechanics in video_pipeline

**Files:**
- Modify: `tauri-client/src-tauri/src/media/video_pipeline.rs`

The pipeline thread receives `SwapEvent`s from the selector and performs the actual swap.

- [ ] **Step 1: Hold the active encoder behind a swappable cell**

In `video_pipeline.rs`, change the encoder ownership pattern. If the pipeline thread currently holds `let mut encoder: VideoEncoder`, change to:

```rust
use std::sync::Arc;
use parking_lot::Mutex;

let active_encoder: Arc<Mutex<VideoEncoder>> = Arc::new(Mutex::new(initial_encoder));
```

(`parking_lot` is already typically in the project; if not, use `std::sync::Mutex`.)

- [ ] **Step 2: On SwapEvent, build new encoder, force keyframe, atomic swap**

In the pipeline's main loop, alongside the existing capture-frame-encode-send cycle, listen on the selector's swap channel:

```rust
loop {
    tokio::select! {
        Some(frame) = capture_rx.recv() => {
            let encoded = {
                let mut enc = active_encoder.lock();
                enc.encode_frame(frame)?
            };
            // ... existing packetize + send logic, stamping codec byte
            // from active_encoder.lock().codec
        }
        Some(swap) = swap_rx.recv() => {
            handle_swap(swap, &active_encoder, &selector, &community_send_tx).await?;
        }
    }
}
```

Implement `handle_swap`:

```rust
async fn handle_swap(
    swap: SwapEvent,
    active_encoder: &Arc<Mutex<VideoEncoder>>,
    selector: &CodecSelector,
    community_send_tx: &mpsc::UnboundedSender<chatproj::Packet>,
) -> Result<(), String> {
    use crate::media::bitrate_preset;

    eprintln!("[codec] swap: {:?} → {:?} ({:?})",
        active_encoder.lock().codec, swap.target.codec, swap.reason);

    // Codec-aware bitrate (spec §8). The Quality value is threaded
    // through the pipeline context — Task 10 wires this. Until then,
    // this Step uses Quality::Medium as a temporary value; do not commit
    // the placeholder beyond Task 10.
    let new_bitrate = bitrate_preset::bitrate_kbps(
        ctx.quality,         // pipeline ctx field added in Task 10 Step 1
        swap.target.codec,
        swap.target.width,
        swap.target.height,
        swap.target.fps,
    );

    let new_config = EncoderConfig {
        width: swap.target.width,
        height: swap.target.height,
        fps: swap.target.fps,
        bitrate_kbps: new_bitrate,
        keyframe_interval_secs: 2,
    };

    // Build new encoder OUT-OF-LOCK so the active encoder keeps producing.
    let new_encoder = VideoEncoder::new(swap.target.codec, &new_config)
        .map_err(|e| format!("failed to build new encoder for swap: {}", e))?;

    // Send StreamCodecChangedNotify BEFORE the swap so the toast appears
    // just as the visual transition starts (spec §6).
    let notify_pkt = build_stream_codec_changed_notify(&swap);
    let _ = community_send_tx.send(notify_pkt);

    // Atomic swap. Old encoder dropped → torn down on this thread; if
    // teardown is heavy, spawn_blocking it instead.
    let old = std::mem::replace(&mut *active_encoder.lock(), new_encoder);
    drop(old);

    // Force a keyframe on the new encoder so viewers can configure
    // their decoder right away.
    active_encoder.lock().force_keyframe();

    selector.record_swap(swap.target);
    Ok(())
}

fn build_stream_codec_changed_notify(swap: &SwapEvent) -> chatproj::Packet {
    let mut pkt = chatproj::Packet::default();
    pkt.r#type = chatproj::packet::Type::StreamCodecChangedNotify as i32;
    pkt.timestamp = chrono::Utc::now().timestamp_millis();
    let notify = chatproj::StreamCodecChangedNotify {
        channel_id: ctx.channel_id.clone(),       // pipeline ctx fields, see substep below
        streamer_username: ctx.streamer_username.clone(),
        new_codec: swap.target.codec as i32,
        new_width: swap.target.width,
        new_height: swap.target.height,
        new_fps: swap.target.fps,
        reason: match swap.reason {
            SwapReason::WatcherJoinedLowCaps => chatproj::stream_codec_changed_notify::Reason::WatcherJoinedLowCaps as i32,
            SwapReason::LimitingWatcherLeft => chatproj::stream_codec_changed_notify::Reason::LimitingWatcherLeft as i32,
            SwapReason::StreamerInitiated => chatproj::stream_codec_changed_notify::Reason::StreamerInitiated as i32,
        },
    };
    pkt.payload = Some(chatproj::packet::Payload::StreamCodecChangedNotify(notify));
    pkt
}
```

- [ ] **Step 2b: Define the pipeline context struct**

The pipeline already needs to remember which channel/streamer it's serving and (added in Task 10) the quality preset. Centralize them in a `StreamerContext` struct that the pipeline owns and the `handle_swap` function reads via `&self` or an explicit parameter:

```rust
pub struct StreamerContext {
    pub channel_id: String,
    pub streamer_username: String,
    pub quality: crate::media::bitrate_preset::Quality, // populated in Task 10
}
```

Construct this once at pipeline start (in `video_pipeline::start`) and either store it on the pipeline struct or pass `Arc<StreamerContext>` to `handle_swap`. Update the `handle_swap` signature accordingly:

```rust
async fn handle_swap(
    ctx: &StreamerContext,
    swap: SwapEvent,
    active_encoder: &Arc<Mutex<VideoEncoder>>,
    selector: &CodecSelector,
    community_send_tx: &mpsc::UnboundedSender<chatproj::Packet>,
) -> Result<(), String>
```

Make the same change to `build_stream_codec_changed_notify` (take `&StreamerContext` instead of having string literals).

- [ ] **Step 3: Listen to watcher events from AppState's broadcast and feed selector**

First, add a Rust-side mirror of voiceStore's `userCapabilities` to `AppState`. The pipeline thread can't make Tauri commands round-trip to React efficiently — it needs direct access to caps data when a watcher joins.

In `tauri-client/src-tauri/src/state.rs`:

```rust
use std::collections::HashMap;
use crate::media::caps::CodecCap;

#[derive(Clone, Default, Debug)]
pub struct PeerCaps {
    pub encode: Vec<CodecCap>,
    pub decode: Vec<CodecCap>,
}

// inside AppState
    /// Mirror of voiceStore.userCapabilities (Plan A Task 17).
    /// Populated when VoicePresenceUpdate arrives.
    pub voice_caps_cache: RwLock<HashMap<String, PeerCaps>>,

// in AppState::new
        voice_caps_cache: RwLock::new(HashMap::new()),
```

In the community connection's `VoicePresenceUpdate` handler (where Plan A Task 17 stores the data in voiceStore via Tauri event), also write to this Rust-side cache:

```rust
let mut cache = state.voice_caps_cache.write().unwrap();
cache.clear();
for (i, username) in update.active_users.iter().enumerate() {
    if let Some(caps) = update.user_capabilities.get(i) {
        cache.insert(username.clone(), PeerCaps {
            encode: caps.encode.iter().map(proto_to_cap).collect(),
            decode: caps.decode.iter().map(proto_to_cap).collect(),
        });
    }
}
```

Where `proto_to_cap` is a small helper:

```rust
fn proto_to_cap(p: &chatproj::CodecCapability) -> CodecCap {
    CodecCap {
        codec: match p.codec {
            1 => CodecKind::H264Hw,
            2 => CodecKind::H264Sw,
            3 => CodecKind::H265,
            4 => CodecKind::Av1,
            _ => CodecKind::Unknown,
        },
        max_width: p.max_width,
        max_height: p.max_height,
        max_fps: p.max_fps,
    }
}
```

Now in the pipeline start function, after creating the `CodecSelector`:

```rust
let mut watcher_rx = state.watcher_event_tx.subscribe();
let selector_arc = Arc::new(selector); // if not already
let selector_for_task = selector_arc.clone();
let voice_caps_cache = state.voice_caps_cache.clone(); // Arc-wrap if needed
let streamer_username = ctx.streamer_username.clone();

tokio::spawn(async move {
    while let Ok(evt) = watcher_rx.recv().await {
        if evt.streamer_username != streamer_username { continue; }
        let watcher_caps = {
            let cache = voice_caps_cache.read().unwrap();
            match cache.get(&evt.watcher_username) {
                Some(c) => c.decode.clone(),
                None => {
                    eprintln!("[codec] watcher caps unknown: {}", evt.watcher_username);
                    continue;
                }
            }
        };
        const JOINED: i32 = 1; // StreamWatcherNotify::JOINED
        const LEFT: i32 = 2;   // StreamWatcherNotify::LEFT
        match evt.action {
            JOINED => selector_for_task.on_watcher_joined(evt.watcher_username, watcher_caps),
            LEFT => selector_for_task.on_watcher_left(&evt.watcher_username),
            _ => {}
        }
    }
});
```

- [ ] **Step 4: Make the codec byte in UdpVideoPacket dynamic per encode**

The `active_encoder.lock().codec` is the source of truth. Update the packetization step in the loop:

```rust
let codec_byte = active_encoder.lock().codec as u8;
let pkt = UdpVideoPacket::new_with_codec(
    sender_id, frame_id, packet_index, total_packets,
    is_keyframe, codec_byte, chunk,
);
```

- [ ] **Step 5: Verify compiles and tests pass**

```bash
cargo check
cargo test --lib
```

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/media/video_pipeline.rs tauri-client/src-tauri/src/state.rs
git commit -m "feat(pipeline): integrate CodecSelector with encoder swap mechanics"
```

---

## Task 8: Server handles StreamCodecChangedNotify

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Add the handler**

Find the packet dispatch (where `START_STREAM_REQ` and other types are handled). Add:

```cpp
} else if (packet.type() == chatproj::Packet::STREAM_CODEC_CHANGED_NOTIFY) {
    if (!packet.has_stream_codec_changed_notify()) continue;
    const auto& notify = packet.stream_codec_changed_notify();
    // Validate sender owns the stream
    if (notify.streamer_username() != session->get_username()) {
        eprintln("[community] STREAM_CODEC_CHANGED_NOTIFY from non-owner ignored");
        continue;
    }
    auto& entry = active_streams_[notify.channel_id()][notify.streamer_username()];
    entry.current_codec = notify.new_codec();
    entry.width = notify.new_width();
    entry.height = notify.new_height();
    entry.fps = notify.new_fps();
    // Rebroadcast presence so all viewers see the new codec on the badge
    // and so their decoders can pre-configure.
    manager_.broadcast_stream_presence(notify.channel_id());
    // Also forward the notify itself so viewers get the toast text/reason.
    manager_.broadcast_to_voice_channel_packet(notify.channel_id(), packet);
}
```

(`broadcast_to_voice_channel_packet` — if no helper exists, add one analogous to existing presence broadcasts, sending the packet to every session in the channel.)

- [ ] **Step 2: Build server**

```bash
cmake --build . --target chatproj_community
```

- [ ] **Step 3: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community): handle StreamCodecChangedNotify, rebroadcast presence + notify"
```

---

## Task 9: React decoder reconfigures on per-packet codec byte change

**Files:**
- Modify: `tauri-client/src/features/voice/StreamVideoPlayer.tsx`

The per-packet codec byte is the wire authority. When it changes, the VideoDecoder must reconfigure. The `currentCodec` from `StreamPresenceUpdate` is just a heads-up — the actual switch happens when the byte arrives.

- [ ] **Step 1: Track the active decoded codec**

In `StreamVideoPlayer.tsx`, add state for the currently-configured decoder codec:

```typescript
const [activeCodec, setActiveCodec] = useState<VideoCodec>(streamInfo.currentCodec);
const decoderRef = useRef<VideoDecoder | null>(null);
```

- [ ] **Step 2: On every incoming encoded frame event, check the codec byte**

Where the player handles encoded-frame events from Tauri:

```typescript
useEffect(() => {
  const unlisten = listen<EncodedFrameEvent>("encoded_frame", (event) => {
    const payload = event.payload;
    if (payload.username !== streamInfo.ownerUsername) return;

    const incomingCodec = payload.codec as VideoCodec;

    if (incomingCodec !== activeCodec) {
      // Codec changed mid-stream. Tear down the existing decoder and
      // configure a new one with the right codec string + description.
      console.log("[player] codec change", activeCodec, "→", incomingCodec);
      decoderRef.current?.close();
      const newDecoder = makeDecoder(canvasRef.current!);
      const description = parseDescription(payload.config_format, payload.config_data);
      newDecoder.configure({
        codec: videoCodecToWebCodecsString(incomingCodec),
        hardwareAcceleration: "prefer-hardware",
        description,
      });
      decoderRef.current = newDecoder;
      setActiveCodec(incomingCodec);
    }

    // Decode the chunk (only if the description was provided OR we already
    // configured — the first packet after a codec change MUST carry the
    // description, which is the encoder's keyframe).
    decoderRef.current?.decode(new EncodedVideoChunk({
      type: payload.keyframe ? "key" : "delta",
      timestamp: payload.timestamp,
      data: base64ToUint8(payload.data),
    }));
  });
  return () => { unlisten.then((fn) => fn()); };
}, [streamInfo.ownerUsername, activeCodec]);
```

- [ ] **Step 3: Ensure the Rust side emits `codec` byte on every event**

In the Tauri event payload (set in Plan B Task 5), make sure `codec: u8` is part of the JSON. If not, add it:

```rust
let payload = serde_json::json!({
    "username": owner,
    "codec": frame.codec as u8,
    "data": base64::encode(&frame.data),
    "timestamp": frame.pts,
    "keyframe": frame.is_keyframe,
    "config_format": config_format,
    "config_data": config_data,
});
```

- [ ] **Step 4: TypeScript compiles + smoke test with Plan B's force_codec**

```bash
npm run build
npm run tauri dev
```

Use the dev `force_codec` to start one stream as AV1, then stop+restart as H.264. Watcher's player should reconfigure decoder cleanly.

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src/features/voice/StreamVideoPlayer.tsx tauri-client/src-tauri/src/events/
git commit -m "feat(player): reconfigure WebCodecs decoder on per-packet codec change"
```

---

## Task 10: Codec-quality preset wired through the pipeline

**Files:**
- Modify: `tauri-client/src-tauri/src/commands/streaming.rs`
- Modify: `tauri-client/src-tauri/src/media/video_pipeline.rs`

- [ ] **Step 1: Parse Quality from string in the command**

In `start_screen_share`, replace any ad-hoc bitrate-from-quality logic with:

```rust
use crate::media::bitrate_preset::{bitrate_kbps, Quality};

let quality = match (quality.as_str(), video_bitrate_kbps) {
    ("low", _)    => Quality::Low,
    ("medium", _) => Quality::Medium,
    ("high", _)   => Quality::High,
    ("custom", Some(kbps)) => Quality::Custom(kbps),
    _ => Quality::Medium,
};

let initial_bitrate_kbps = bitrate_kbps(quality, codec, width, height, fps);
```

Pass `quality` into `video_pipeline::start(..., quality, ...)` so it can be used in `handle_swap` (resolves Task 7's TODO).

- [ ] **Step 2: Use `quality` in handle_swap**

Replace the hardcoded `Quality::Medium` in `handle_swap`:

```rust
let new_bitrate = bitrate_preset::bitrate_kbps(
    self.quality,    // pass via pipeline ctx
    swap.target.codec,
    swap.target.width,
    swap.target.height,
    swap.target.fps,
);
```

- [ ] **Step 3: Verify compiles + run all tests**

```bash
cargo check
cargo test --lib
```

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/commands/streaming.rs tauri-client/src-tauri/src/media/video_pipeline.rs
git commit -m "feat(quality): codec-aware bitrate preset wired into pipeline + swap"
```

---

## Task 11: StreamConfigDialog — Codec dropdown

**Files:**
- Modify: `tauri-client/src/features/voice/StreamConfigDialog.tsx` (or `CaptureSourcePicker.tsx`)

- [ ] **Step 1: Add the dropdown control**

Find the existing dialog and add a new control alongside resolution/fps/quality:

```tsx
import { useCodecSettingsStore } from "../../stores/codecSettingsStore";
import { VideoCodec } from "../../types";

// ... inside the component
const { encodeCaps } = useCodecSettingsStore();
const [enforcedCodec, setEnforcedCodec] = useState<VideoCodec>(VideoCodec.UNKNOWN);

const codecOptions: { value: VideoCodec; label: string }[] = [
  { value: VideoCodec.UNKNOWN, label: "Auto (recommended)" },
  ...encodeCaps.map((c) => ({
    value: c.codec,
    label: `Force ${codecLabelFor(c.codec)}`,
  })),
];

// in JSX
<label>
  Codec
  <select
    value={enforcedCodec}
    onChange={(e) => setEnforcedCodec(Number(e.target.value) as VideoCodec)}
    title="Forcing a codec prevents viewers without that decoder from watching this stream."
  >
    {codecOptions.map((o) => (
      <option key={o.value} value={o.value}>{o.label}</option>
    ))}
  </select>
</label>
```

Add the `codecLabelFor` helper near the top:

```tsx
function codecLabelFor(c: VideoCodec): string {
  switch (c) {
    case VideoCodec.AV1: return "AV1";
    case VideoCodec.H265: return "H.265";
    case VideoCodec.H264_HW: return "H.264";
    case VideoCodec.H264_SW: return "H.264 (software)";
    default: return "Auto";
  }
}
```

- [ ] **Step 2: Pass `enforcedCodec` through to start_screen_share**

When the user clicks "Start streaming", invoke:

```typescript
await invoke("start_screen_share", {
  // ... existing params
  enforcedCodec: enforcedCodec === VideoCodec.UNKNOWN ? null : enforcedCodec,
});
```

- [ ] **Step 3: Replace dev `force_codec` with production `enforced_codec`**

In `tauri-client/src-tauri/src/commands/streaming.rs`, rename the parameter:

```rust
pub async fn start_screen_share(
    // ...
    enforced_codec: Option<u8>,
    // remove force_codec
)
```

And use it:

```rust
let enforced = match enforced_codec {
    None | Some(0) => None,
    Some(1) => Some(CodecKind::H264Hw),
    Some(2) => Some(CodecKind::H264Sw),
    Some(3) => Some(CodecKind::H265),
    Some(4) => Some(CodecKind::Av1),
    _ => return Err("invalid codec".into()),
};

// Initial codec choice:
let chosen_codec = match enforced {
    Some(c) => c,
    None => {
        // Auto: pick the streamer's top encode codec, filtered by toggles.
        let encode = caps::get_or_probe_encoders();
        let toggles = Toggles {
            use_av1: *state.use_av1.read().unwrap(),
            use_h265: *state.use_h265.read().unwrap(),
        };
        let initial = StreamSettings { codec: CodecKind::Unknown, width, height, fps };
        codec_selection::pick(&encode, &[], &toggles, initial).codec
    }
};
```

- [ ] **Step 4: Pass `enforced_codec` and `chosen_codec` into the StartStreamRequest**

Update the `StartStreamRequest` build:

```rust
let req = StartStreamRequest {
    // ... existing
    chosen_codec: chosen_codec as i32,
    enforced_codec: enforced.map(|c| c as i32).unwrap_or(0),
};
```

And construct the `CodecSelector` with the enforced state:

```rust
let (selector, swap_rx) = CodecSelector::new(initial_settings, enforced, encode_caps, toggles);
```

- [ ] **Step 5: Verify compiles + smoke test**

```bash
npm run tauri dev
```

Open stream config, see "Codec" dropdown with available encoders. Pick "Auto" (default) → starts stream. Pick "Force AV1" → stream uses AV1, no auto-renegotiation.

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src/features/voice/StreamConfigDialog.tsx tauri-client/src-tauri/src/commands/streaming.rs
git commit -m "feat(stream): production Codec dropdown (Auto + Force) replacing dev shim"
```

---

## Task 12: Watch button gating in stream presence list

**Files:**
- Modify: `tauri-client/src/features/voice/StreamViewPanel.tsx` (or wherever per-stream entries render)

- [ ] **Step 1: Add a helper that decides if the local user can watch**

In a shared utility (e.g., `tauri-client/src/utils/canWatchStream.ts`):

```typescript
import { VideoCodec, type CodecCapability, type StreamInfo } from "../types";

export function canWatchStream(
  stream: StreamInfo,
  localDecodeCaps: CodecCapability[],
): { canWatch: boolean; reason?: string } {
  const has = (codec: VideoCodec) => localDecodeCaps.some((c) => c.codec === codec);

  const codecLabel = (c: VideoCodec): string => {
    switch (c) {
      case VideoCodec.AV1: return "AV1";
      case VideoCodec.H265: return "H.265 / HEVC";
      case VideoCodec.H264_HW:
      case VideoCodec.H264_SW: return "H.264";
      default: return "this codec";
    }
  };

  if (stream.enforcedCodec !== VideoCodec.UNKNOWN) {
    if (!has(stream.enforcedCodec)) {
      return {
        canWatch: false,
        reason: `Cannot decode ${codecLabel(stream.enforcedCodec)} — your hardware/browser doesn't support it (streamer has locked this codec).`,
      };
    }
  }
  if (stream.currentCodec !== VideoCodec.UNKNOWN && !has(stream.currentCodec)) {
    return {
      canWatch: false,
      reason: `Cannot decode ${codecLabel(stream.currentCodec)} — your hardware/browser doesn't support it.`,
    };
  }
  return { canWatch: true };
}
```

- [ ] **Step 2: Use the helper in the stream entry render**

Find the component that renders each stream entry with a Watch button. Update:

```tsx
import { canWatchStream } from "../../utils/canWatchStream";
import { useCodecSettingsStore } from "../../stores/codecSettingsStore";

const { decodeCaps } = useCodecSettingsStore();
const { canWatch, reason } = canWatchStream(stream, decodeCaps);

<button
  disabled={!canWatch}
  onClick={canWatch ? () => onWatch(stream) : undefined}
  title={reason}
  className={canWatch ? "watch-btn" : "watch-btn watch-btn--disabled"}
>
  Watch
</button>
```

- [ ] **Step 3: Style the disabled state**

In the relevant CSS / Tailwind config, add a faded look:

```css
.watch-btn--disabled {
  opacity: 0.4;
  cursor: not-allowed;
}
```

- [ ] **Step 4: Verify TypeScript compiles**

```bash
npm run build
```

- [ ] **Step 5: Smoke test**

Stream with `enforced_codec = AV1` from one client. From a second client without AV1 decode (test by removing AV1 from `localStorage["decibell.decoder_caps.v1"]` and reloading), confirm Watch button is grayed with hover tooltip.

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src/utils/canWatchStream.ts tauri-client/src/features/voice/StreamViewPanel.tsx tauri-client/src/styles/
git commit -m "feat(voice): gray out Watch button when local client can't decode stream"
```

---

## Task 13: CodecBadge component on the player tile

**Files:**
- Create: `tauri-client/src/features/voice/CodecBadge.tsx`
- Modify: `tauri-client/src/features/voice/StreamVideoPlayer.tsx`

- [ ] **Step 1: Create the badge component**

Create `tauri-client/src/features/voice/CodecBadge.tsx`:

```tsx
import { VideoCodec } from "../../types";

interface Props {
  codec: VideoCodec;
  width: number;
  height: number;
  fps: number;
  enforced: boolean;
}

const CODEC_COLOR: Record<number, string> = {
  [VideoCodec.AV1]:    "#7C3AED", // purple
  [VideoCodec.H265]:   "#2563EB", // blue
  [VideoCodec.H264_HW]: "#0EA5E9", // teal
  [VideoCodec.H264_SW]: "#6B7280", // gray
};

const CODEC_LABEL: Record<number, string> = {
  [VideoCodec.AV1]:    "AV1",
  [VideoCodec.H265]:   "H.265",
  [VideoCodec.H264_HW]: "H.264",
  [VideoCodec.H264_SW]: "H.264 SW",
};

export function CodecBadge({ codec, width, height, fps, enforced }: Props) {
  const color = CODEC_COLOR[codec] ?? "#6B7280";
  const label = CODEC_LABEL[codec] ?? "—";
  // Resolution label: prefer the conventional shorthand (1080p, 1440p, 4K)
  // when it matches a common preset, else width×height.
  const resLabel = formatResolution(width, height);

  return (
    <div
      className="codec-badge"
      style={{
        position: "absolute",
        top: 8,
        right: 8,
        display: "inline-flex",
        alignItems: "center",
        gap: 6,
        padding: "4px 10px",
        borderRadius: 999,
        background: "rgba(0,0,0,0.55)",
        color: "white",
        fontSize: 12,
        fontWeight: 600,
        backdropFilter: "blur(4px)",
      }}
    >
      <span style={{ color: "#cbd5e1" }}>{resLabel}{fps}</span>
      <span style={{ color }}>{label}</span>
      {enforced && (
        <span
          aria-label="Codec locked by streamer"
          title={`Streamer has locked this stream to ${label}.`}
          style={{ color: "#FBBF24" }}
        >
          {/* FontAwesome lock glyph if FA is loaded; else inline SVG */}
          {"\u{f023}"}
        </span>
      )}
    </div>
  );
}

function formatResolution(w: number, h: number): string {
  if (w === 3840 && h === 2160) return "4K";
  if (w === 2560 && h === 1440) return "1440p";
  if (w === 1920 && h === 1080) return "1080p";
  if (w === 1280 && h === 720)  return "720p";
  return `${w}×${h}`;
}
```

- [ ] **Step 2: Render the badge in StreamVideoPlayer**

In `StreamVideoPlayer.tsx`, alongside the `<canvas>`:

```tsx
import { CodecBadge } from "./CodecBadge";

// ... in render
<div style={{ position: "relative" }}>
  <canvas ref={canvasRef} ... />
  <CodecBadge
    codec={streamInfo.currentCodec}
    width={streamInfo.resolutionWidth}
    height={streamInfo.resolutionHeight}
    fps={streamInfo.fps}
    enforced={streamInfo.enforcedCodec !== VideoCodec.UNKNOWN}
  />
</div>
```

- [ ] **Step 3: TypeScript compiles + smoke test**

```bash
npm run build
npm run tauri dev
```

Watch a stream. Badge appears top-right with "1080p60 H.264" (or whatever). Force a codec via Codec dropdown — badge updates and lock icon appears.

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src/features/voice/CodecBadge.tsx tauri-client/src/features/voice/StreamVideoPlayer.tsx
git commit -m "feat(voice): codec/quality badge with lock icon on stream player"
```

---

## Task 14: Toast notifications on codec change

**Files:**
- Create: `tauri-client/src/utils/codecToasts.ts`
- Modify: wherever the React side handles incoming `STREAM_CODEC_CHANGED_NOTIFY` events from Tauri

- [ ] **Step 1: Build the toast text helper**

Create `tauri-client/src/utils/codecToasts.ts`:

```typescript
import { VideoCodec, StreamCodecChangeReason, type StreamCodecChangedNotify } from "../types";

function codecLabel(c: VideoCodec): string {
  switch (c) {
    case VideoCodec.AV1: return "AV1";
    case VideoCodec.H265: return "H.265";
    case VideoCodec.H264_HW: return "H.264";
    case VideoCodec.H264_SW: return "H.264 (software)";
    default: return "Unknown";
  }
}

function resLabel(w: number, h: number, fps: number): string {
  const r = w === 3840 && h === 2160 ? "4K"
    : w === 2560 && h === 1440 ? "1440p"
    : w === 1920 && h === 1080 ? "1080p"
    : w === 1280 && h === 720  ? "720p"
    : `${w}×${h}`;
  return `${r}${fps}`;
}

export function buildCodecToast(
  notify: StreamCodecChangedNotify,
  forLocalUserIsStreamer: boolean,
): { text: string } | null {
  const codec = codecLabel(notify.newCodec);
  const res = resLabel(notify.newWidth, notify.newHeight, notify.newFps);
  const triggerWatcher = "watcher_username" in (notify as any) ? (notify as any).watcher_username : undefined;

  switch (notify.reason) {
    case StreamCodecChangeReason.WATCHER_JOINED_LOW_CAPS:
      return {
        text: forLocalUserIsStreamer
          ? `Switched to ${codec} at ${res} so a viewer can watch.`
          : `${notify.streamerUsername} switched to ${codec} (${res}).`
      };
    case StreamCodecChangeReason.LIMITING_WATCHER_LEFT:
      return {
        text: forLocalUserIsStreamer
          ? `Restored to ${codec} at ${res}.`
          : `${notify.streamerUsername} restored to ${codec} (${res}).`
      };
    case StreamCodecChangeReason.STREAMER_INITIATED:
      return {
        text: forLocalUserIsStreamer
          ? `Codec changed to ${codec} at ${res}.`
          : `${notify.streamerUsername} switched to ${codec} (${res}).`
      };
    default:
      return null;
  }
}
```

- [ ] **Step 2: Use the existing toastStore to display it**

Open `tauri-client/src/stores/toastStore.ts` to confirm the API (likely `addToast({ text, duration })` or similar). In the React handler that processes incoming `STREAM_CODEC_CHANGED_NOTIFY`:

```typescript
import { useToastStore } from "../../stores/toastStore";
import { buildCodecToast } from "../../utils/codecToasts";
import { useAuthStore } from "../../stores/authStore";

// In the event handler:
const { addToast } = useToastStore.getState();
const { username: localUser } = useAuthStore.getState();
const isStreamer = notify.streamerUsername === localUser;
const toast = buildCodecToast(notify, isStreamer);
if (toast) {
  addToast({ text: toast.text, duration: 4000, position: "top-center" });
}
```

(Adapt to the toast store's actual API. If `position` isn't supported, ignore it — top-center is the spec default but visual placement is configurable later.)

- [ ] **Step 3: Wire the listener**

Find where Tauri events are subscribed (likely an `App.tsx` `useEffect` or per-feature wiring). Add:

```typescript
import { listen } from "@tauri-apps/api/event";

useEffect(() => {
  const unlisten = listen<StreamCodecChangedNotify>("stream_codec_changed", (e) => {
    handleCodecChange(e.payload);
  });
  return () => { unlisten.then((fn) => fn()); };
}, []);
```

(Confirm the Rust side emits this event when receiving the notify packet from the server — Plan A's net layer should forward proto events to React. If not yet wired, add the emit in the community connection's dispatch.)

- [ ] **Step 4: TypeScript compiles + smoke test**

```bash
npm run build
npm run tauri dev
```

With three accounts: A streams (Auto codec, AV1-capable), B watches (AV1-capable). Then C joins voice without AV1 decode and clicks Watch on A's stream. After ~200ms, toasts appear on A, B, and C: "Switched to H.264 at 1440p60 so [C] can watch." (or similar — the watcher username comes from the server's notify enrichment if added; otherwise text is generic).

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src/utils/codecToasts.ts tauri-client/src/App.tsx
git commit -m "feat(voice): toast notifications for codec change events"
```

---

## Task 15: Server defensive WatchStreamRequest rejection on enforcement

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Reject incompatible WatchStreamRequest under enforcement**

In the `WATCH_STREAM_REQ` handler, before adding the watcher:

```cpp
const auto& req = packet.watch_stream_req();
auto& entry = active_streams_[req.channel_id()][req.target_username()];

if (entry.enforced_codec != chatproj::CODEC_UNKNOWN) {
    // Look up the watcher's caps
    auto watcher_caps = session->get_capabilities();
    bool can_decode = false;
    for (const auto& c : watcher_caps.decode()) {
        if (c.codec() == entry.enforced_codec) {
            can_decode = true;
            break;
        }
    }
    if (!can_decode) {
        // Silently drop the request — the client UI should have grayed
        // the button. Log for diagnostics.
        eprintln("[community] dropping WATCH_STREAM_REQ from %s "
                 "(can't decode enforced codec %d)",
                 session->get_username().c_str(),
                 (int)entry.enforced_codec);
        continue; // skip the rest of the handler
    }
}
// ... existing watcher-add logic
```

- [ ] **Step 2: Build server**

```bash
cmake --build . --target chatproj_community
```

- [ ] **Step 3: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community): defensively reject WatchStreamRequest under enforcement when watcher lacks decoder"
```

---

## Task 16: End-to-end smoke test for full negotiation

- [ ] **Step 1: Three-client setup**

Start the community server, then three Tauri clients (A, B, C) with different accounts. Confirm Plan A's `voiceStore.userCapabilities` populates correctly for all three when they join voice.

- [ ] **Step 2: Test auto-pick on stream start**

A starts streaming with Auto (no enforcement). All three should support AV1 decode (assuming reasonable hardware). Confirm:
- A's badge shows "<res> AV1".
- B and C see the same badge.
- A's `[encoder]` log shows AV1 encoder selected.

- [ ] **Step 3: Test downgrade on watcher join**

In a fourth client D, with AV1 decode disabled (manually clear AV1 from localStorage decoder caps and reload), join the voice channel. Confirm:
- A receives a `StreamWatcherNotify` event in logs.
- After ~200ms, A's CodecSelector fires a swap event.
- A's `[codec] swap` log shows the transition.
- A toast appears on all four clients.
- A, B, C, D's badges update to show H.264 (or H.265) at the new resolution.
- The actual video continues without total breakage (a brief freeze is expected).

- [ ] **Step 4: Test upgrade after limiting watcher leaves**

D leaves the voice channel. Confirm:
- A receives a `StreamWatcherNotify(LEFT)` event.
- 30s pass, no swap fires before then.
- After 30s, swap fires back to AV1.
- Badges update on A, B, C.

- [ ] **Step 5: Test enforcement**

A stops streaming, then starts again with "Force AV1". Confirm:
- A's badge shows AV1 + lock icon.
- D's stream-list entry shows grayed Watch button with tooltip.
- D clicking Watch (if somehow possible via DevTools) does nothing visible — server drops it.

- [ ] **Step 6: Commit any tweaks**

```bash
git add -u
git commit -m "fix: end-to-end Plan C smoke test fixes"
```

---

## Task 17: Final verification checklist

Cross-check each spec section against implemented behavior:

- [ ] §5.1 LCD picker — Task 5 (with tests)
- [ ] §5.2 stream start picks codec from streamer encode caps + toggles — Task 11
- [ ] §5.3 200ms debounce on watcher-join downgrade — Task 6 (test)
- [ ] §5.4 30s cooldown on upgrade, monotonic, cancellable — Task 6 (tests)
- [ ] §5.5 enforcement short-circuits auto-logic + server-side reject — Tasks 6, 11, 15
- [ ] §6 swap mechanics: build new encoder, force keyframe, atomic swap, send notify before swap — Task 7
- [ ] §7.2 enforce dropdown — Task 11
- [ ] §7.3 watch button gating — Task 12
- [ ] §7.4 codec/quality badge with lock icon — Task 13
- [ ] §7.5 toast notifications — Task 14
- [ ] §8 codec-aware bitrate preset table — Tasks 4, 10
- [ ] StreamWatcherNotify forwarding — Tasks 1, 2, 3
- [ ] StreamCodecChangedNotify handling on server — Task 8
- [ ] React per-packet decoder reconfigure — Task 9

Once all verified, the codec-negotiation feature ships as designed.
