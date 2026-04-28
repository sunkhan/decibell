# Windows D3D11 Zero-Copy Streaming Pipeline — Design Spec

**Date:** 2026-04-28
**Status:** Approved for implementation planning
**Scope:** Sender-side video pipeline on Windows. NVIDIA NVENC encoders only (AMD/Intel = follow-up work). Both DXGI Desktop Duplication (monitor capture) and Windows Graphics Capture (window capture). Receiver side and Linux are unchanged.

---

## 1. Goal

Eliminate the GPU→CPU→GPU round-trip in the Windows streaming pipeline by wiring DXGI/WGC capture, BGRA→NV12 color conversion, and NVENC encoding into a single shared D3D11 device. Frames stay in VRAM from capture through encode; only the (already-small) compressed bitstream crosses back to CPU for UDP packetization.

For 1440p60 streaming this eliminates ~310 MB/s of memory bandwidth per active stream and the corresponding CPU time. Aligns directly with the project's "low resource usage, high optimization" north star and the "0 CPU load preferred" goal stated by the user.

Linux already has equivalent zero-copy paths (`new_cuda` for NVIDIA via DMA-BUF + CUDA, `new_vaapi` for AMD/Intel). This spec brings Windows up to parity for the NVIDIA case.

---

## 2. Scope

**In scope:**
- Single new `GpuStreamingPipeline` for Windows that owns a shared D3D11 device and runs capture + conversion + encode on one thread
- Two GPU capture sources: `DxgiSource` (monitor) and `WgcSource` (window) — both produce BGRA D3D11 textures
- `H264Encoder::new_d3d11` constructor + `encode_d3d11_frame` submission method, mirroring the existing `new_cuda` pattern
- Silent runtime fallback to today's CPU-readback pipeline if any D3D11 init step fails, with a toast notification to the user
- libx264 software encoder always uses the existing CPU-readback path (no D3D11 attempt)

**Out of scope (deferred to follow-up):**
- AMD AMF / Intel QSV D3D11VA paths (same plumbing pattern, vendor-specific options dictionaries)
- Receiver-side OffscreenCanvas/worker-thread optimization
- Linux changes (the existing CUDA/VA-API paths stay)
- Stream audio capture optimization
- Any change to the wire format, codec negotiation logic, or React decoder side

---

## 3. Architecture

```
              CaptureSource (enum)
              ├── DxgiSource (monitor capture)
              └── WgcSource  (window capture)
                            │
                            ▼ BGRA D3D11 texture
              ┌──────────────────────────────────┐
              │  VideoProcessor                  │
              │  BGRA → NV12 (writes into        │
              │  pool-allocated NV12 texture)    │
              └──────────────────────────────────┘
                            │
                            ▼ NV12 D3D11 texture (in encoder pool)
              ┌──────────────────────────────────┐
              │  NvencD3d11Encoder               │
              │  Submit AVFrame, drain bitstream │
              └──────────────────────────────────┘
                            │
                            ▼ encoded bytes (CPU)
                            ▼ packetize + UDP send (existing)
```

Single thread runs the whole loop (the existing `decibell-video` thread). The current two-thread `capture_dxgi → mpsc → video_pipeline` model is collapsed for the GPU path because NVENC has its own internal pipelining and we don't need cross-thread back-pressure for textures that share a device. (The CPU-readback path keeps the two-thread model for libx264/fallback.)

Zero readbacks. The only data crossing back to CPU is the encoded bitstream — same as today.

---

## 4. D3D11 Device Ownership + Sharing

The pipeline owns one `Arc<ID3D11Device>`. All three components — capture, converter, encoder — get a reference and never create their own.

```rust
struct GpuStreamingPipeline {
    device: Arc<ID3D11Device>,
    context: ID3D11DeviceContext,
    capture: CaptureSource,    // DxgiSource | WgcSource
    converter: VideoProcessor, // BGRA → NV12 blitter
    encoder: NvencD3d11Encoder,
}
```

**Device creation:** `D3D11CreateDevice` with `D3D11_CREATE_DEVICE_BGRA_SUPPORT` (already required by the existing VideoProcessor). For DXGI capture, the device must be on the same adapter as the target output (this is the `create_device_for_adapter` helper that already exists in `capture_dxgi.rs`). For WGC, the adapter is arbitrary — we use whichever the chosen source's window is on.

**Per-component integration:**

- **DxgiSource** — already takes a device; just becomes "use the shared one" instead of creating its own.
- **WgcSource** — currently creates its own internal device. Refactor: accept an external `ID3D11Device`, wrap it as `IDirect3DDevice` via `IDirect3DDxgiInterfaceAccess` for `Direct3D11CaptureFramePool::CreateFreeThreaded`.
- **VideoProcessor** — already takes a device. Reuse pattern.
- **NvencD3d11Encoder** — gets the device wrapped in an `AVHWDeviceContext` (D3D11VA flavor) via `av_hwdevice_ctx_alloc(AV_HWDEVICE_TYPE_D3D11VA)` + manual populate of `AVD3D11VADeviceContext.device` + `av_hwdevice_ctx_init`. Same pattern as the existing `new_cuda` constructor for CUDA.

**Why one device, not separate-with-keyed-mutex:** D3D11 textures are device-bound — a texture from device A can't be sampled by device B without serialized cross-device sync (`IDXGIKeyedMutex`). One device removes that complexity entirely. The trade-off — any GPU hang or device-removed event takes down the whole pipeline — is acceptable because that's a single-stream catastrophic failure either way; nothing functional is lost vs today.

---

## 5. NVENC + FFmpeg D3D11VA Integration

New constructor and frame-submission method on `H264Encoder` (the struct retains its name from Plan B for diff hygiene; despite the name it produces all four codecs):

```rust
#[cfg(target_os = "windows")]
impl H264Encoder {
    pub fn new_d3d11(
        target_codec: CodecKind,
        config: &EncoderConfig,
        shared_device: &ID3D11Device,
    ) -> Result<Self, String> { ... }

    pub fn encode_d3d11_frame(
        &mut self,
        nv12_texture: &ID3D11Texture2D,  // pool-allocated, already filled by VideoProcessor
    ) -> Result<Option<EncodedFrame>, String> { ... }
}
```

**`new_d3d11` steps:**
1. Wrap `shared_device` in `AVHWDeviceContext` (D3D11VA type) via `av_hwdevice_ctx_alloc` → manually populate the `device` field of `AVD3D11VADeviceContext` → `av_hwdevice_ctx_init`. Mirror the `new_cuda` pattern with `AV_HWDEVICE_TYPE_D3D11VA` instead of `AV_HWDEVICE_TYPE_CUDA`.
2. Build `hw_frames_ctx` for a pool of NV12 D3D11 textures (`AV_PIX_FMT_D3D11` software-format, `AV_PIX_FMT_NV12` hardware-format, pool size ~4 textures sized `width × height`).
3. Find the codec via the existing `find_hw_encoder` (`av1_nvenc` / `hevc_nvenc` / `h264_nvenc`), set `pix_fmt = AV_PIX_FMT_D3D11`, attach `hw_device_ctx` and `hw_frames_ctx` to the encoder context.
4. Same options dictionary as today's NVENC path (preset p5, tune ull/ll for HEVC, rc cbr, slices=1 for HEVC, VBV buffer size).
5. `open_with(opts)`.

**Frame submission flow (in the pipeline loop):**
1. `let av_frame = encoder.acquire_pool_frame()?` — calls `av_hwframe_get_buffer(hw_frames_ctx, frame, 0)`. The frame's `data[0]` is a pointer to the pool-managed `ID3D11Texture2D`.
2. `converter.blit_into(bgra_input_texture, av_frame.texture())` — `VideoProcessor` writes BGRA→NV12 directly into the pool texture; no extra copy.
3. `encoder.submit(av_frame)` — calls `encoder.send_frame(&av_frame)` then drains via the existing `receive_one_packet`.

`EncodedFrame` is unchanged. Description-record extraction (avcC/hvcC/av1C) is unchanged. Per-packet codec byte stamp is unchanged. The change is *purely* about how the encoder consumes input frames.

---

## 6. Capture Refactor

Both DXGI and WGC need to (a) accept the shared `ID3D11Device`, (b) hand out BGRA D3D11 textures.

```rust
pub struct GpuFrame {
    pub texture: ID3D11Texture2D,  // BGRA, in shared device's VRAM
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
}

pub trait GpuCaptureSource {
    fn next_frame(&mut self) -> Result<Option<GpuFrame>, CaptureError>;
}
```

**`capture_dxgi.rs` changes:**
- Add `DxgiSource` struct implementing `GpuCaptureSource`. Holds the `IDXGIOutputDuplication` handle. `next_frame()` calls `AcquireNextFrame`, casts the resource to `ID3D11Texture2D`, returns `GpuFrame { texture, ... }`. Caller releases the frame after the converter is done with it (texture acquired this iteration).
- Remove `convert_and_readback` and the staging texture from this path — the `VideoProcessor` moves out into the pipeline.
- Remove the dedicated capture thread for the GPU path. The pipeline pulls from `next_frame()` directly in its main loop.
- The existing `start_capture` returning `Receiver<RawFrame>` (the CPU-readback path) stays in the file unchanged — used by the libx264 fallback and the silent-fallback path from Section 7.

**`capture_wgc.rs` changes:**
- Add `WgcSource` struct implementing `GpuCaptureSource`. Constructor accepts an external `ID3D11Device`, wraps as `IDirect3DDevice` for `Direct3D11CaptureFramePool::CreateFreeThreaded`. `next_frame()` calls `frame_pool.TryGetNextFrame()`, extracts the surface, gets `ID3D11Texture2D` via `IDirect3DDxgiInterfaceAccess::GetInterface`, returns the `GpuFrame`.
- Existing `start_capture` returning `Receiver<RawFrame>` stays, same as DXGI.

**Source routing:**
`capture::list_sources()` already prefixes IDs (`monitor:adapter_idx:output_idx` for DXGI, `window:hwnd` for WGC). The pipeline picks `DxgiSource` or `WgcSource` based on prefix. No change to the source-selection UI.

---

## 7. Fallback + User Notification

**Failure points (caught at stream startup, before first encoded packet ships):**
1. `D3D11CreateDevice` fails (driver too old, no D3D11-capable GPU)
2. `av_hwdevice_ctx_init` for D3D11VA fails, or `hw_frames_ctx` allocation fails, or `open_with` fails on the D3D11-configured encoder
3. First `av_hwframe_get_buffer` or `send_frame` call fails

On any failure:
- Tear down anything we partially built (release D3D11 textures, drop hw_frames_ctx, drop hw_device_ctx, drop the encoder, drop the device)
- Construct the existing CPU-readback pipeline: `capture_dxgi::start_capture` (or `capture_wgc::start_capture`) returning `Receiver<RawFrame>` + `H264Encoder::new` taking CPU NV12
- Emit a Tauri event `STREAM_GPU_FALLBACK` carrying the underlying error string
- Stream proceeds normally on the CPU path — visually identical, just higher CPU usage

**Toast wording (React side, `useVoiceEvents.ts` listener):**
- *Title:* "GPU encoding unavailable"
- *Body:* "Streaming via CPU path — higher CPU usage. {error}"
- *Severity:* warning, duration 6 seconds

**libx264 software encoder always uses CPU path:**
When the user picks "Force H.264 (software)" (or LCD picker resolves to H264_SW), `start_screen_share` skips the D3D11 attempt entirely and uses the existing CPU readback path directly. No toast — this is the expected behavior for that codec. The auto-pick LCD picker won't choose libx264 unless no hardware encoder is in the streamer's caps list.

**Selection logic (in `start_screen_share`):**

```
let codec = ... (resolved per Plan C codec selection)

if codec ∈ {H264Hw, H265, AV1}:
    try GpuStreamingPipeline::start(target_codec, source_id, encoder_config)
    if Ok(pipeline) → use it
    if Err(e) → emit STREAM_GPU_FALLBACK(e), construct existing CPU pipeline

else (codec == H264Sw):
    construct existing CPU pipeline directly (no D3D11 attempt)
```

---

## 8. Verification + Code Layout

**How we verify "preferably 0 CPU load":**
Three measurements before/after on the dev machine streaming 1440p60 to a single viewer:
1. **CPU usage of `decibell.exe`** in Task Manager — expect existing path ~10–25% (color conversion + readback + re-upload), expect new path ~1–3% (just packetization + UDP)
2. **Memory bandwidth** via `nvidia-smi --query-gpu=memory.used,utilization.memory --format=csv` — should drop because no system→GPU upload per frame
3. **End-to-end frame latency** — should be roughly equal or slightly lower (no readback wait)

These aren't automated tests; they're "open Task Manager, start a stream, eyeball it" verifications you do once after the implementation lands. Not blocking on hitting specific numbers — directional improvements are the success criterion.

**Unit tests:** the new `H264Encoder::new_d3d11` constructor isn't easily unit-testable (requires a real D3D11 device + NVENC license + matching NVIDIA driver). Test discipline is the existing `cargo test --lib caps::tests::probe_encoders_does_not_panic` pattern — confirm the module compiles and constructor doesn't panic when a no-op probe is run. Behavior verification via interactive `npm run tauri dev` smoke test.

**Code layout (file-level summary):**

| File | Action | Responsibility |
|---|---|---|
| `media/gpu_pipeline.rs` | Create | `GpuStreamingPipeline` orchestrator: owns shared D3D11 device, runs the single-thread capture→convert→encode loop |
| `media/capture_dxgi.rs` | Modify | Add `DxgiSource` (GPU type) alongside existing `start_capture` (CPU type) |
| `media/capture_wgc.rs` | Modify | Add `WgcSource` (GPU type) alongside existing `start_capture` (CPU type); accept external D3D11 device |
| `media/encoder.rs` | Modify | Add `H264Encoder::new_d3d11` + `encode_d3d11_frame` + helpers; mirror `new_cuda` pattern |
| `media/video_pipeline.rs` | Modify | Selection logic per Section 7; on hw codec, try `GpuStreamingPipeline` first |
| `commands/streaming.rs` | Modify | Wire the fallback selection; pass `target_codec` through to the pipeline factory |
| `events/mod.rs` | Modify | New event constant + payload: `STREAM_GPU_FALLBACK` |
| `net/community.rs` | No change | Wire layer untouched |
| `caps.rs` | No change | Encoder probe still uses the CPU init path (which always works); D3D11 zero-copy is a runtime upgrade, not a capability declaration |
| React `useVoiceEvents.ts` | Modify | Listen for `stream_gpu_fallback`, push toast |

---

## 9. Out-of-scope (deferred)

- **AMD AMF / Intel QSV D3D11VA paths** — same plumbing pattern, vendor-specific options dictionaries. Deferred to a follow-up spec once NVIDIA path is stable.
- **Receiver-side optimization** (`OffscreenCanvas` + worker thread) — different concern from sender-side GPU work; deserves its own brainstorm.
- **Linux changes** — existing `new_cuda` and `new_vaapi` paths stay untouched.
- **HEVC partial-picture rendering bug** — separate ongoing investigation; not affected by this work.
- **Codec negotiation behavior** — the LCD picker, swap mechanics, and toggle UI from Plan C all keep working as-is. This spec doesn't touch them.

---

## 10. Notes for the Implementation Plan

The implementation order that minimizes risk:

1. **`H264Encoder::new_d3d11` constructor + frame submission** — mostly mechanical translation of the existing `new_cuda` pattern. Verify it builds; smoke-test by feeding it a hand-allocated D3D11 NV12 texture (test shim), confirm encoded packets come out.
2. **Capture refactor** — add `DxgiSource` + `WgcSource` alongside existing CPU-path code. Keep both in the file. No behavior change for existing CPU path.
3. **`GpuStreamingPipeline`** — wire capture + VideoProcessor + encoder together using the shared device. Single-thread loop. Verify by replacing the existing pipeline-thread launch with the new one for hardware codecs.
4. **Fallback wiring** — try-catch the GPU pipeline construction in `start_screen_share`; on failure, fall back to the existing path and emit the event.
5. **React toast** — listener + toast wording in `useVoiceEvents.ts`.
6. **Verification** — run the three measurements from Section 8.

Each step is independently testable. Steps 1–3 don't ship any user-visible change yet (the new path is opt-in via the selection logic added in step 4); only step 4 actually flips the runtime to use the GPU path for hardware codecs.
