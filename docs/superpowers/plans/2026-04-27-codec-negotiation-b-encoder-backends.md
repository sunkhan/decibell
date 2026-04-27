# Codec Negotiation — Plan B: Encoder & Decoder Backends

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Depends on:** Plan A (capability plumbing — must be merged first).

**Goal:** Add HEVC, AV1, and x264 software encoder backends to `encoder.rs`, plus the WebCodecs decoder configuration per codec on the React side. Add a dev-only force-codec parameter so each codec can be smoke-tested end-to-end before Plan C wires up auto-negotiation.

**Architecture:** Refactor the existing `H264Encoder` struct into a `VideoEncoder` parameterized by `CodecKind`. Each codec has its own list of FFmpeg encoder name candidates, codec-specific config tuning (preset, profile, level, bitrate control mode), and codec-specific description-record extraction (avcC for H.264, hvcC for H.265, av1C for AV1). The React `StreamVideoPlayer` reads the per-stream codec from `StreamPresenceUpdate.current_codec` and configures WebCodecs `VideoDecoder` with the appropriate codec string and description format.

**Tech Stack:** `ffmpeg-next` 8 (encoders: NVENC, AMF, QSV, MF for HW; libx264, libsvtav1, libx265 for SW), WebCodecs `VideoDecoder`, `avc1.*` / `hev1.*` / `av01.*` codec strings.

**Spec reference:** `docs/superpowers/specs/2026-04-27-video-codec-negotiation-design.md` §§ 3.1, 3.2 (encode policy ceilings drive Plan A's probe but encoder construction still uses the user's chosen settings clamped to those ceilings), 7.2 (force-codec via dropdown — UI ships in Plan C, dev shim ships here).

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `tauri-client/src-tauri/src/media/encoder.rs` | Modify | `VideoEncoder` (renamed from `H264Encoder`), takes `CodecKind`, switches encoder name candidates + per-codec config + description-record extraction |
| `tauri-client/src-tauri/src/media/video_pipeline.rs` | Modify | Construct `VideoEncoder` with codec parameter; pass codec byte through to packets |
| `tauri-client/src-tauri/src/commands/streaming.rs` | Modify | Accept dev-only `force_codec: Option<u8>` parameter on `start_screen_share`, pass through |
| `tauri-client/src-tauri/Cargo.toml` | Possibly modify | Document FFmpeg feature requirements; add comments listing required encoders |
| `tauri-client/src/features/voice/StreamVideoPlayer.tsx` | Modify | Switch decoder codec string based on `currentCodec`; handle hvcC / av1C description blobs |
| `tauri-client/src/utils/codecMap.ts` | Create | Tiny helper: `videoCodecToWebCodecsString(codec)` |
| `tauri-client/src/features/voice/CaptureSourcePicker.tsx` (or wherever start-stream UI lives) | Modify | Accept dev-only force-codec selection (hidden behind a config flag or a dev-only menu — visible UI ships in Plan C) |

---

## Task 1: Verify FFmpeg build has required encoders

**Files:** none modified — this is a verification + documentation task.

The FFmpeg library bundled with `ffmpeg-next` is configured at build time. On Windows this is typically vcpkg's `ffmpeg` port. We need to ensure the following encoders are linked:

- `libx264` (H.264 software)
- `libx265` is **not required** — we use hardware H.265 only. (HEVC software encoding is too CPU-heavy to fit the project's "low resource" goal.)
- `libsvtav1` or `libaom-av1` (AV1 software, optional — only used as a fallback for AV1 hardware probing in dev; production AV1 path is hardware-only)
- Hardware encoders are part of FFmpeg's standard build: `nvenc`, `amf`, `qsv`, `mediafoundation` (Windows), `vaapi` (Linux)

- [ ] **Step 1: Run the encoder probe and inspect output**

Build and run the existing test added in Plan A Task 7:

```bash
# from tauri-client/src-tauri/
cargo test --lib caps::tests::probe_encoders_does_not_panic -- --nocapture
```

Expected output includes `[caps] encoder available: H264Sw via libx264`. If it does not, x264 is missing from the FFmpeg build.

- [ ] **Step 2: If x264 is missing on Windows, fix vcpkg port**

```bash
# Reinstall the ffmpeg port with the x264 feature.
vcpkg install "ffmpeg[x264,nvcodec,amf,qsv]:x64-windows"
# Rebuild the Rust crate (clean to force linker re-evaluation)
cd tauri-client/src-tauri
cargo clean -p decibell
cargo build
```

Verify the probe now reports x264 available.

- [ ] **Step 3: Document FFmpeg requirements in Cargo.toml**

Append a comment block to `tauri-client/src-tauri/Cargo.toml` near the `ffmpeg-next` line:

```toml
ffmpeg-next = "8"
# Required FFmpeg encoders (verify in vcpkg/system FFmpeg build):
#   - libx264          (H.264 software, codec value 2)
#   - h264_nvenc / h264_amf / h264_qsv / h264_mf   (H.264 hardware, codec value 1)
#   - hevc_nvenc / hevc_amf / hevc_qsv / hevc_mf   (H.265 hardware, codec value 3)
#   - av1_nvenc  / av1_amf  / av1_qsv              (AV1 hardware,  codec value 4)
# Linux additionally uses h264_vaapi, hevc_vaapi, av1_vaapi where present.
# vcpkg install: ffmpeg[x264,nvcodec,amf,qsv]:x64-windows
```

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/Cargo.toml
git commit -m "docs(cargo): document FFmpeg encoder requirements for codec negotiation"
```

---

## Task 2: Refactor H264Encoder to VideoEncoder with codec parameter (no behavior change)

**Files:**
- Modify: `tauri-client/src-tauri/src/media/encoder.rs`
- Modify: `tauri-client/src-tauri/src/media/video_pipeline.rs`

The goal of this task is purely structural: rename + add a `codec: CodecKind` field. All existing call sites still pass `CodecKind::H264Hw`, behavior is identical.

- [ ] **Step 1: Rename the struct and add a codec field**

In `tauri-client/src-tauri/src/media/encoder.rs`, replace:

```rust
pub struct H264Encoder {
    // existing fields
}
```

with:

```rust
use crate::media::caps::CodecKind;

pub struct VideoEncoder {
    pub codec: CodecKind,
    // existing fields
}
```

Add a public type alias to keep call sites compiling during the transition:

```rust
#[deprecated(note = "Use VideoEncoder")]
pub type H264Encoder = VideoEncoder;
```

- [ ] **Step 2: Add codec parameter to the constructor**

Locate the constructor (likely `H264Encoder::new(config: &EncoderConfig) -> Result<Self, ...>`). Add a `codec: CodecKind` parameter as the first argument:

```rust
impl VideoEncoder {
    pub fn new(codec: CodecKind, config: &EncoderConfig) -> Result<Self, String> {
        // existing body, but store `codec` on self
    }
}
```

In the body, store `codec` on self at construction time. For now, ignore the codec value internally — the existing H.264 encoder candidate list runs unconditionally.

- [ ] **Step 3: Update the call site(s)**

Search:
```bash
grep -rn "H264Encoder::new" tauri-client/src-tauri/src/
```

Update each call site to pass `CodecKind::H264Hw`:

```rust
let encoder = VideoEncoder::new(CodecKind::H264Hw, &config)?;
```

- [ ] **Step 4: Verify compiles + tests pass**

```bash
cargo check
cargo test --lib
```
Expected: passes. The deprecation warning on `H264Encoder` is fine.

- [ ] **Step 5: Smoke-test a stream**

```bash
npm run tauri dev
```
Start a stream, watch a stream from a second account, confirm video flows. (No codec change yet — H.264 hardware is still what's used.)

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/media/encoder.rs tauri-client/src-tauri/src/media/video_pipeline.rs
git commit -m "refactor(encoder): rename H264Encoder to VideoEncoder, add codec parameter"
```

---

## Task 3: Switch encoder candidate list on codec

**Files:**
- Modify: `tauri-client/src-tauri/src/media/encoder.rs`

- [ ] **Step 1: Extract the encoder candidate list into a codec-aware helper**

Inside `VideoEncoder::new` (or a free function in the same file), replace the hardcoded list with:

```rust
fn encoder_name_candidates(codec: CodecKind) -> &'static [&'static str] {
    match codec {
        CodecKind::H264Hw => &["h264_nvenc", "h264_vaapi", "h264_amf", "h264_qsv", "h264_mf"],
        CodecKind::H264Sw => &["libx264"],
        CodecKind::H265   => &["hevc_nvenc", "hevc_vaapi", "hevc_amf", "hevc_qsv", "hevc_mf"],
        CodecKind::Av1    => &["av1_nvenc", "av1_vaapi", "av1_amf", "av1_qsv"],
        CodecKind::Unknown => &[],
    }
}
```

In the body where the encoder is opened, iterate over this list:

```rust
let mut chosen = None;
for name in encoder_name_candidates(codec) {
    if let Some(c) = ffmpeg::encoder::find_by_name(name) {
        // try open_as ...
        if open_succeeded {
            chosen = Some((c, *name));
            break;
        }
    }
}
let (codec_ffmpeg, name) = chosen.ok_or_else(|| format!("No encoder for codec {:?}", codec))?;
eprintln!("[encoder] using {} for {:?}", name, codec);
```

- [ ] **Step 2: Verify compiles**

```bash
cargo check
```

- [ ] **Step 3: Smoke-test H.264 hardware still works**

```bash
npm run tauri dev
```
Start a stream — should still use a hardware H.264 encoder (NVENC etc.). Confirm with `[encoder] using h264_nvenc for H264Hw` log line.

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/encoder.rs
git commit -m "feat(encoder): codec-aware candidate list (still H.264 only at runtime)"
```

---

## Task 4: Per-codec config tuning

**Files:**
- Modify: `tauri-client/src-tauri/src/media/encoder.rs`

Each codec has its own set of FFmpeg options that produce good streaming behavior (low latency, CBR, sane GOP). Centralize them in a per-codec helper.

- [ ] **Step 1: Add a per-codec configuration helper**

In `encoder.rs`:

```rust
/// Apply codec-specific FFmpeg options to an encoder context.
fn apply_codec_options(
    codec: CodecKind,
    encoder_name: &str,
    enc: &mut ffmpeg_next::encoder::Video,
) {
    use ffmpeg_next::Dictionary;
    let mut opts = Dictionary::new();

    match codec {
        CodecKind::H264Hw => match encoder_name {
            "h264_nvenc" => {
                opts.set("preset", "p4");           // p1=fastest, p7=highest quality; p4=balanced
                opts.set("tune", "ull");            // ultra-low-latency
                opts.set("rc", "cbr");
                opts.set("zerolatency", "1");
                opts.set("delay", "0");
            }
            "h264_amf" => {
                opts.set("usage", "ultralowlatency");
                opts.set("rc", "cbr");
                opts.set("quality", "speed");
            }
            "h264_qsv" => {
                opts.set("preset", "veryfast");
                opts.set("forced_idr", "1");
            }
            "h264_mf" => {
                opts.set("scenario", "display_remoting");
                opts.set("rate_control", "cbr");
            }
            _ => {}
        },
        CodecKind::H264Sw => {
            // libx264 — zero-latency streaming preset
            opts.set("preset", "veryfast");
            opts.set("tune", "zerolatency");
            opts.set("x264opts", "no-scenecut");
            opts.set("rc", "cbr");
            // Force NAL HRD = CBR for proper bitrate adherence over UDP
            opts.set("nal-hrd", "cbr");
        }
        CodecKind::H265 => match encoder_name {
            "hevc_nvenc" => {
                opts.set("preset", "p4");
                opts.set("tune", "ull");
                opts.set("rc", "cbr");
                opts.set("zerolatency", "1");
                opts.set("delay", "0");
            }
            "hevc_amf" => {
                opts.set("usage", "ultralowlatency");
                opts.set("rc", "cbr");
            }
            "hevc_qsv" => {
                opts.set("preset", "veryfast");
                opts.set("forced_idr", "1");
            }
            "hevc_mf" => {
                opts.set("scenario", "display_remoting");
                opts.set("rate_control", "cbr");
            }
            _ => {}
        },
        CodecKind::Av1 => match encoder_name {
            "av1_nvenc" => {
                opts.set("preset", "p4");
                opts.set("tune", "ull");
                opts.set("rc", "cbr");
                opts.set("zerolatency", "1");
            }
            "av1_amf" => {
                opts.set("usage", "ultralowlatency");
                opts.set("rc", "cbr");
            }
            "av1_qsv" => {
                opts.set("preset", "veryfast");
            }
            _ => {}
        },
        CodecKind::Unknown => {}
    }

    // Apply via open_as_with — caller passes opts. We can't apply them
    // post-open, so the caller signature changes too.
    let _ = (opts, enc); // tied together at the call site
}
```

Actually the FFmpeg API requires options at `open_as` time (you pass `Dictionary`). So restructure: have this helper *return* the `Dictionary` rather than apply it.

```rust
fn codec_options(
    codec: CodecKind,
    encoder_name: &str,
) -> ffmpeg_next::Dictionary {
    use ffmpeg_next::Dictionary;
    let mut opts = Dictionary::new();
    // ... same body as above but populating `opts` and returning it
    opts
}
```

Then at the call site in `VideoEncoder::new`:

```rust
let opts = codec_options(codec, name);
encoder_ctx.open_as_with(codec_ffmpeg, opts)?;
```

(Verify the exact `ffmpeg-next` API name — it might be `open_with` or `open_as_with`. Adjust as needed.)

- [ ] **Step 2: Verify compiles**

```bash
cargo check
```

- [ ] **Step 3: Smoke-test H.264 hardware still works**

```bash
npm run tauri dev
```
Start a stream, confirm video.

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/encoder.rs
git commit -m "feat(encoder): per-codec FFmpeg option tuning (CBR, low-latency)"
```

---

## Task 5: Per-codec description record extraction

**Files:**
- Modify: `tauri-client/src-tauri/src/media/encoder.rs`

WebCodecs requires a description record on decoder construction:
- H.264 → AVCC (`AVCDecoderConfigurationRecord`) — built from SPS+PPS NALU.
- H.265 → hvcC (`HEVCDecoderConfigurationRecord`) — built from VPS+SPS+PPS NALU.
- AV1 → av1C (`AV1CodecConfigurationRecord`) — built from sequence header OBU.

FFmpeg encoders typically expose the description record in `extradata` after the first frame, in the format expected for the codec. We just need to copy it out.

- [ ] **Step 1: Update EncodedFrame to label the description format**

In `encoder.rs`, find the `EncodedFrame` struct (likely already has `avcc_description: Option<Vec<u8>>`). Rename and broaden:

```rust
#[derive(Clone, Debug)]
pub enum DecoderConfig {
    Avcc(Vec<u8>),
    Hvcc(Vec<u8>),
    Av1c(Vec<u8>),
}

pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub pts: i64,
    pub decoder_config: Option<DecoderConfig>,
    pub codec: CodecKind,
}
```

Update all call sites that read `frame.avcc_description` to match the new shape (use `match`).

- [ ] **Step 2: Build the description record at the right moment**

After the encoder opens and produces its first frame, FFmpeg's `encoder.codec_context().extradata()` contains the codec's bitstream-format-specific config (avcC / hvcC / av1C). Copy it out:

```rust
fn extract_decoder_config(
    codec: CodecKind,
    enc: &ffmpeg_next::codec::Context,
) -> Option<DecoderConfig> {
    let extradata = enc.extradata()?;
    if extradata.is_empty() { return None; }
    let bytes = extradata.to_vec();
    Some(match codec {
        CodecKind::H264Hw | CodecKind::H264Sw => DecoderConfig::Avcc(bytes),
        CodecKind::H265 => DecoderConfig::Hvcc(bytes),
        CodecKind::Av1 => DecoderConfig::Av1c(bytes),
        CodecKind::Unknown => return None,
    })
}
```

In the encode loop, after the first keyframe, set `frame.decoder_config = extract_decoder_config(self.codec, &self.encoder_context);`. For non-keyframes, leave it `None` — viewers only need the description on the first decode configuration.

- [ ] **Step 3: Verify compiles**

```bash
cargo check
```

- [ ] **Step 4: Update event emit so React gets the format-tagged description**

Find where `EncodedFrame` is emitted to React via Tauri event (look for `emit_all("encoded_frame", ...)` or similar). Update the JSON payload to include the format tag:

```rust
let (config_format, config_data) = match &frame.decoder_config {
    Some(DecoderConfig::Avcc(d)) => ("avcc", Some(base64::encode(d))),
    Some(DecoderConfig::Hvcc(d)) => ("hvcc", Some(base64::encode(d))),
    Some(DecoderConfig::Av1c(d)) => ("av1c", Some(base64::encode(d))),
    None => ("none", None),
};
```

Include both in the payload.

- [ ] **Step 5: Verify compiles, smoke test**

```bash
cargo check
npm run tauri dev
```
Stream H.264 hardware; confirm video flows and that the first event has `config_format: "avcc"`.

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/media/encoder.rs tauri-client/src-tauri/src/events/
git commit -m "feat(encoder): per-codec decoder config extraction (avcC/hvcC/av1C)"
```

---

## Task 6: WebCodecs decoder string mapping (React)

**Files:**
- Create: `tauri-client/src/utils/codecMap.ts`
- Modify: `tauri-client/src/features/voice/StreamVideoPlayer.tsx`

- [ ] **Step 1: Create the codec → WebCodecs string helper**

Create `tauri-client/src/utils/codecMap.ts`:

```typescript
import { VideoCodec } from "../types";

/// Maps a VideoCodec to a WebCodecs codec string suitable for
/// VideoDecoder.configure({ codec: ... }). The string used here is the
/// "common case" — for H.264/H.265 the actual stream may use a different
/// profile/level, but the description record from extradata fully
/// describes the stream and the decoder accepts any compatible variant
/// once configured.
export function videoCodecToWebCodecsString(c: VideoCodec): string {
  switch (c) {
    case VideoCodec.AV1:    return "av01.0.05M.08";   // Main profile, level 5.1, 8-bit
    case VideoCodec.H265:   return "hev1.1.6.L120.B0"; // Main profile, level 4.0
    case VideoCodec.H264_HW:
    case VideoCodec.H264_SW: return "avc1.640033";    // High profile, level 5.1
    default: return "avc1.640033"; // safe fallback
  }
}

export function isCodecHwAcceleratable(_c: VideoCodec): boolean {
  // The WebCodecs configure() call we make uses
  // hardwareAcceleration: "prefer-hardware" — the browser/webview
  // decides per stream, so we don't gate manually.
  return true;
}
```

- [ ] **Step 2: Update StreamVideoPlayer to use the helper**

Open `tauri-client/src/features/voice/StreamVideoPlayer.tsx`. Find the `decoder.configure({ codec: "avc1.640033", ... })` call. Replace with:

```typescript
import { videoCodecToWebCodecsString } from "../../utils/codecMap";
// ...
const codecString = videoCodecToWebCodecsString(streamInfo.currentCodec);
const description = parseDescription(event.config_format, event.config_data);
decoder.configure({
  codec: codecString,
  hardwareAcceleration: "prefer-hardware",
  description, // Uint8Array from base64 of avcC/hvcC/av1C bytes
});
```

Where `parseDescription` is:

```typescript
function parseDescription(format: string, base64: string | null): Uint8Array | undefined {
  if (!base64 || format === "none") return undefined;
  // All three formats (avcc, hvcc, av1c) hand the bytes directly to the
  // decoder via the description field — same shape, different parser.
  return Uint8Array.from(atob(base64), (c) => c.charCodeAt(0));
}
```

- [ ] **Step 3: Verify TypeScript compiles**

```bash
npm run build
```

- [ ] **Step 4: Smoke-test H.264 streaming still works**

```bash
npm run tauri dev
```
Start a stream from one account, watch from another. Should look identical to before — the codec path is dynamically chosen but resolves to H.264 since that's the only codec being produced.

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src/utils/codecMap.ts tauri-client/src/features/voice/StreamVideoPlayer.tsx
git commit -m "feat(player): switch WebCodecs decoder string based on stream's currentCodec"
```

---

## Task 7: Dev-only force-codec parameter on start_screen_share

**Files:**
- Modify: `tauri-client/src-tauri/src/commands/streaming.rs`
- Modify: `tauri-client/src-tauri/src/media/video_pipeline.rs`

This task adds a developer back-door for selecting any codec without yet shipping the production UI. Plan C will surface the production "Force codec" dropdown.

- [ ] **Step 1: Add a force_codec parameter to start_screen_share**

In `tauri-client/src-tauri/src/commands/streaming.rs`, find `start_screen_share` and add an optional parameter:

```rust
#[tauri::command]
pub async fn start_screen_share(
    server_id: String,
    channel_id: String,
    source_id: String,
    resolution: String,
    fps: u32,
    quality: String,
    video_bitrate_kbps: Option<u32>,
    share_audio: bool,
    audio_bitrate_kbps: Option<u32>,
    force_codec: Option<u8>,  // dev/QA shim — Plan C will replace with `enforced_codec`
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<(), String> {
    // ... existing parse/validate code

    let codec = match force_codec {
        Some(0) | None => CodecKind::H264Hw, // legacy default
        Some(1) => CodecKind::H264Hw,
        Some(2) => CodecKind::H264Sw,
        Some(3) => CodecKind::H265,
        Some(4) => CodecKind::Av1,
        Some(_) => return Err(format!("Unknown codec value: {:?}", force_codec)),
    };

    // Pass codec to the pipeline construction:
    let video_pipeline = video_pipeline::start(
        codec,
        encoder_config,
        // ... rest
    )?;
    // ...
}
```

- [ ] **Step 2: Plumb codec through video_pipeline::start**

In `tauri-client/src-tauri/src/media/video_pipeline.rs`, update the entry point:

```rust
pub fn start(
    codec: CodecKind,
    encoder_config: EncoderConfig,
    // ... existing args
) -> Result<...> {
    let encoder = VideoEncoder::new(codec, &encoder_config)?;
    // ... rest of the function passes `codec` into the packetizer so
    //    UdpVideoPacket.codec is stamped correctly.
}
```

- [ ] **Step 3: Stamp correct codec byte in UdpVideoPacket**

Find where `UdpVideoPacket::new` is called (likely in `video_pipeline.rs` or `video_packet.rs`). The constructor today hardcodes `codec: CODEC_H264_HW`. Update the call site to pass the active codec:

```rust
let pkt = UdpVideoPacket::new_with_codec(
    sender_id,
    frame_id,
    packet_index,
    total_packets,
    is_keyframe,
    codec_byte,      // <-- new
    chunk_data,
);
```

And add a new constructor in `video_packet.rs`:

```rust
impl UdpVideoPacket {
    pub fn new_with_codec(
        sender_id_str: &str,
        frame_id: u32,
        packet_index: u16,
        total_packets: u16,
        is_keyframe: bool,
        codec: u8,
        data: &[u8],
    ) -> Self {
        let mut pkt = Self::new(sender_id_str, frame_id, packet_index, total_packets, is_keyframe, data);
        pkt.codec = codec;
        pkt
    }
}
```

(Or modify the existing constructor signature — the new helper preserves back-compat if other callers exist.)

Map `CodecKind → codec_byte` via `codec as u8`.

- [ ] **Step 4: Pipe StartStreamRequest.chosen_codec through to the server**

In `start_screen_share`, when building the `StartStreamRequest` for sending over TCP, set:

```rust
let req = StartStreamRequest {
    channel_id,
    target_fps: fps as i32,
    target_bitrate_kbps: video_bitrate_kbps as i32,
    has_audio: share_audio,
    resolution_width,
    resolution_height,
    chosen_codec: codec as i32,
    enforced_codec: 0, // CODEC_UNKNOWN — Plan C will populate this from UI
};
```

- [ ] **Step 5: Verify compiles**

```bash
cargo check
```

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/commands/streaming.rs tauri-client/src-tauri/src/media/video_pipeline.rs tauri-client/src-tauri/src/media/video_packet.rs
git commit -m "feat(streaming): dev-only force_codec parameter on start_screen_share"
```

---

## Task 8: End-to-end smoke test for each codec

For each codec, the streamer manually sets `force_codec` via DevTools, the React player decodes it.

- [ ] **Step 1: Build and run two clients**

```bash
# in two separate shells, each with a different account
npm run tauri dev
```

- [ ] **Step 2: Test H.264 hardware (force_codec = 1)**

In the streamer's DevTools console:
```javascript
await window.__TAURI__.core.invoke("start_screen_share", {
  serverId: "<id>", channelId: "<channel>", sourceId: "<source>",
  resolution: "720p", fps: 30, quality: "medium",
  videoBitrateKbps: 4000, shareAudio: false, audioBitrateKbps: 128,
  forceCodec: 1
});
```
Watch from the second client. Confirm video appears.

- [ ] **Step 3: Test H.264 software (force_codec = 2)**

Stop the previous stream. Repeat with `forceCodec: 2`. Streamer logs should show `[encoder] using libx264 for H264Sw`. Confirm video appears on viewer; CPU usage on streamer is noticeably higher than hardware H.264.

- [ ] **Step 4: Test H.265 (force_codec = 3)**

Repeat with `forceCodec: 3`. Streamer logs should show `[encoder] using hevc_nvenc for H265` (or AMF/QSV/MF). Confirm viewer renders correctly. If viewer shows blank video, check the WebCodecs config — the description record format must match (`hev1.*` codec string, hvcC description bytes from extradata).

- [ ] **Step 5: Test AV1 (force_codec = 4)**

Repeat with `forceCodec: 4`. If the streamer's GPU lacks AV1 encode, the encoder construction will fail with "No encoder for codec Av1" — that's expected; skip the test on hardware that doesn't support it.

- [ ] **Step 6: Document any tweaks needed**

Each codec may need encoder-option fine-tuning discovered during smoke testing. Apply fixes inline to `encoder.rs`.

- [ ] **Step 7: Commit any fixes**

```bash
git add -u
git commit -m "fix(encoder): smoke-test fixes for HEVC/AV1/x264 encoder paths"
```

---

## Task 9: Plan B verification checklist

- [ ] Each codec produces a valid stream that the React WebCodecs player decodes:
  - [ ] H.264 hardware (CodecKind::H264Hw, byte 1)
  - [ ] H.264 software (CodecKind::H264Sw, byte 2)
  - [ ] H.265 (CodecKind::H265, byte 3) — on at least one of NVENC/AMF/QSV/MF
  - [ ] AV1 (CodecKind::Av1, byte 4) — on at least one supported GPU; failure to find an encoder is acceptable on older hardware
- [ ] `UdpVideoPacket.codec` byte correctly stamped per encoder (verify by capturing a packet in Wireshark or logging on the server side temporarily)
- [ ] WebCodecs decoder configures with the right codec string + description per codec (verify by checking the player initializes without errors for each)
- [ ] No auto-negotiation behavior yet — codec is whatever the streamer forced via dev parameter

This plan ships:
- Four working encoder backends behind the `VideoEncoder` abstraction.
- React player that adapts to whichever codec the streamer chose.
- Dev-only force-codec selector for QA before Plan C ships the production negotiation logic.
