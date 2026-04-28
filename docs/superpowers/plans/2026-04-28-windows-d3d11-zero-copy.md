# Windows D3D11 Zero-Copy Streaming Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the GPU→CPU→GPU round-trip in the Windows streaming pipeline for NVENC encoders. Captured BGRA texture stays in VRAM through color conversion and encode; only the encoded bitstream crosses back to CPU.

**Architecture:** Single shared `ID3D11Device` owned by a new `GpuStreamingPipeline`. Capture (DXGI or WGC) produces BGRA D3D11 textures. `VideoProcessor` blits BGRA→NV12 directly into NVENC's `hw_frames_ctx`-managed pool texture. NVENC encodes via FFmpeg's D3D11VA hwaccel. Single-thread loop replaces the existing `capture → mpsc → pipeline` two-thread model for the GPU path. CPU-readback path stays intact for libx264 and as runtime fallback.

**Tech Stack:** `windows` crate 0.61 (Direct3D11, DXGI, Graphics.Capture), `ffmpeg-next` 8 with D3D11VA hwaccel (`av_hwdevice_ctx_alloc(AV_HWDEVICE_TYPE_D3D11VA)` + `hw_frames_ctx`), Tauri events for fallback notification.

**Spec reference:** `docs/superpowers/specs/2026-04-28-windows-d3d11-zero-copy-design.md`

---

## File Structure

| File | Action | Responsibility |
|---|---|---|
| `tauri-client/src-tauri/src/media/encoder.rs` | Modify | Add `H264Encoder::new_d3d11` + `acquire_pool_frame` + `encode_d3d11_frame`. Mirror `new_cuda` pattern. |
| `tauri-client/src-tauri/src/media/gpu_capture.rs` | Create | `GpuFrame` struct + `GpuCaptureSource` trait + shared D3D11 device creation helper. |
| `tauri-client/src-tauri/src/media/capture_dxgi.rs` | Modify | Add `DxgiSource` (GPU type) alongside existing `start_capture` (CPU type, unchanged). Promote `create_device_for_adapter` to `pub`. |
| `tauri-client/src-tauri/src/media/capture_wgc.rs` | Modify | Add `WgcSource` (GPU type) alongside existing `start_capture` (CPU type, unchanged). Reuse existing `create_winrt_device`. |
| `tauri-client/src-tauri/src/media/video_processor.rs` | Create | Move `VideoProcessor` out of `capture_dxgi.rs` into its own module. Add `blit_into(src_bgra, dst_nv12)` that writes into a caller-provided NV12 texture (the encoder pool's texture). |
| `tauri-client/src-tauri/src/media/gpu_pipeline.rs` | Create | `GpuStreamingPipeline` orchestrator: owns shared device, runs single-thread capture→convert→encode loop. Replaces `run_video_send_pipeline` for the GPU path. |
| `tauri-client/src-tauri/src/media/mod.rs` | Modify | `pub mod gpu_capture; pub mod video_processor; pub mod gpu_pipeline;` |
| `tauri-client/src-tauri/src/commands/streaming.rs` | Modify | Selection logic: hardware codec → try GpuStreamingPipeline → fallback to CPU pipeline + emit event. libx264 → CPU pipeline directly. |
| `tauri-client/src-tauri/src/events/mod.rs` | Modify | New `STREAM_GPU_FALLBACK` event constant + payload type + emitter. |
| `tauri-client/src/features/voice/useVoiceEvents.ts` | Modify | Listen for `stream_gpu_fallback`, push warning toast. |

---

## Task 1: Make `create_device_for_adapter` public + extract `VideoProcessor` to its own module

The shared D3D11 device creation lives in `capture_dxgi.rs` as a private helper. Promote it to `pub` and move `VideoProcessor` to `media/video_processor.rs` so the new pipeline can construct both without depending on capture internals.

**Files:**
- Modify: `tauri-client/src-tauri/src/media/capture_dxgi.rs`
- Create: `tauri-client/src-tauri/src/media/video_processor.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

- [ ] **Step 1: Add module declaration**

In `tauri-client/src-tauri/src/media/mod.rs`, add (Windows only — VideoProcessor uses D3D11):

```rust
#[cfg(target_os = "windows")]
pub mod video_processor;
```

- [ ] **Step 2: Promote `create_device_for_adapter` to public**

In `capture_dxgi.rs`, find the function declaration (currently around line 173):

```rust
fn create_device_for_adapter(
```

Change to:

```rust
pub fn create_device_for_adapter(
```

- [ ] **Step 3: Move `VideoProcessor` struct, impl, and helpers**

Cut `VideoProcessor` (the struct + `impl`) out of `capture_dxgi.rs`. Paste into the new file `tauri-client/src-tauri/src/media/video_processor.rs`. At the top of the new file, add the imports the moved code references (find them in `capture_dxgi.rs`'s import block — `windows::core::Interface`, the D3D11 / Dxgi / Foundation imports).

Skeleton of the new file:

```rust
//! BGRA → NV12 GPU color conversion via D3D11 Video Processor.
//! Owns no D3D11 device — the device is passed in by the caller.

use windows::core::Interface;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

pub struct VideoProcessor {
    // ... move struct fields here
}

unsafe impl Send for VideoProcessor {}

impl VideoProcessor {
    pub fn new(
        device: &ID3D11Device,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<Self, String> {
        // ... move new() body here, BUT remove the staging_texture creation
        // (the encoder pool now owns the destination texture; we no longer
        // need a private staging texture for readback)
        // ... remove the nv12_texture creation too — destination comes from pool
    }

    /// Blit BGRA source into a caller-provided NV12 destination texture.
    /// The destination is the encoder's hw_frames_ctx pool texture.
    pub fn blit_into(
        &self,
        context: &ID3D11DeviceContext,
        bgra_texture: &ID3D11Texture2D,
        nv12_dst: &ID3D11Texture2D,
    ) -> Result<(), String> {
        // ... will be filled in Task 2
        unimplemented!()
    }
}
```

- [ ] **Step 4: Replace `capture_dxgi.rs`'s use of VideoProcessor with a re-export**

In `capture_dxgi.rs`, after the moved code is gone, add at the top of the file:

```rust
use super::video_processor::VideoProcessor;
```

The existing `start_capture` (CPU path) keeps using `VideoProcessor` — but now it imports from the new module. Existing `convert_and_readback` becomes a method on... wait, this is getting complex. To minimize disruption to the existing CPU path:

Keep a SECOND copy of the old `VideoProcessor` (the one with the staging texture and `convert_and_readback` method) inside `capture_dxgi.rs`, renamed to `LegacyVideoProcessor`. The new minimal `VideoProcessor` in `video_processor.rs` is the GPU-pipeline-friendly version.

Specifically: in `capture_dxgi.rs`, rename the existing `struct VideoProcessor` to `struct LegacyVideoProcessor`, and rename `impl VideoProcessor` to `impl LegacyVideoProcessor`. Update all references in `capture_dxgi.rs` (the `VideoProcessor::new` calls and `.convert_and_readback` calls) to use `LegacyVideoProcessor`.

That gives us two distinct types, each focused on one path. The GPU pipeline never touches `LegacyVideoProcessor`.

- [ ] **Step 5: Stub `blit_into` so it compiles**

In `video_processor.rs`, replace `unimplemented!()` in `blit_into` with a temporary `Ok(())`:

```rust
pub fn blit_into(
    &self,
    context: &ID3D11DeviceContext,
    bgra_texture: &ID3D11Texture2D,
    nv12_dst: &ID3D11Texture2D,
) -> Result<(), String> {
    let _ = (context, bgra_texture, nv12_dst);
    Ok(()) // implemented in Task 2
}
```

- [ ] **Step 6: Verify it compiles**

```bash
cd tauri-client/src-tauri
cargo check
```

Expected: compiles with warnings only (the new `VideoProcessor::blit_into` is unused so far).

- [ ] **Step 7: Commit**

```bash
git add tauri-client/src-tauri/src/media/capture_dxgi.rs \
        tauri-client/src-tauri/src/media/video_processor.rs \
        tauri-client/src-tauri/src/media/mod.rs
git commit -m "refactor(media): extract VideoProcessor to its own module, keep LegacyVideoProcessor for CPU path"
```

---

## Task 2: Implement `VideoProcessor::blit_into` writing to an external NV12 texture

The new method does the same `VideoProcessorBlt` operation as before, but the output view is built per-call against the caller-supplied destination texture (the encoder's pool texture changes per frame).

**Files:**
- Modify: `tauri-client/src-tauri/src/media/video_processor.rs`

- [ ] **Step 1: Implement `blit_into`**

Replace the stub `blit_into` body in `video_processor.rs`:

```rust
pub fn blit_into(
    &self,
    context: &ID3D11DeviceContext,
    bgra_texture: &ID3D11Texture2D,
    nv12_dst: &ID3D11Texture2D,
) -> Result<(), String> {
    unsafe {
        // Build input view for the BGRA source (per frame because the
        // captured texture changes each iteration).
        let input_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
            },
        };
        let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
        self.video_device
            .CreateVideoProcessorInputView(
                bgra_texture,
                &self.enumerator,
                &input_view_desc,
                Some(&mut input_view),
            )
            .map_err(|e| format!("CreateVideoProcessorInputView: {}", e))?;
        let input_view = input_view.ok_or("CreateVideoProcessorInputView returned None")?;

        // Build output view for the destination NV12 texture (per frame
        // because the encoder pool hands out a different texture each call).
        let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
            },
        };
        let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
        self.video_device
            .CreateVideoProcessorOutputView(
                nv12_dst,
                &self.enumerator,
                &output_view_desc,
                Some(&mut output_view),
            )
            .map_err(|e| format!("CreateVideoProcessorOutputView: {}", e))?;
        let output_view = output_view.ok_or("CreateVideoProcessorOutputView returned None")?;

        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: BOOL(1),
            OutputIndex: 0,
            InputFrameOrField: 0,
            PastFrames: 0,
            FutureFrames: 0,
            ppPastSurfaces: std::ptr::null_mut(),
            pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
            ppFutureSurfaces: std::ptr::null_mut(),
            ppPastSurfacesRight: std::ptr::null_mut(),
            pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
            ppFutureSurfacesRight: std::ptr::null_mut(),
        };

        self.video_context
            .VideoProcessorBlt(
                &self.processor,
                &output_view,
                0,
                std::slice::from_ref(&stream),
            )
            .map_err(|e| format!("VideoProcessorBlt: {}", e))?;

        // Release the COM ref ManuallyDrop kept alive
        std::mem::ManuallyDrop::into_inner(std::ptr::read(&stream.pInputSurface));

        // Note: no readback. The blit writes into the caller's nv12_dst
        // texture which lives in the encoder's hw_frames_ctx pool.
        let _ = context;
        Ok(())
    }
}
```

- [ ] **Step 2: Remove the now-unused `nv12_texture` field from the new `VideoProcessor`**

In `video_processor.rs`'s `VideoProcessor::new`, find the block that creates `nv12_texture` (the one we destination-blitted into in the old code) — it's no longer needed because each call's destination comes from the caller. Delete that block and remove the `nv12_texture` field from the struct.

Same for any `staging_texture` field — those were for readback, not needed.

- [ ] **Step 3: Verify it compiles**

```bash
cargo check
```

Expected: compiles. Some `unused_imports` warnings if a now-unused windows feature import lingers — clean those up.

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/video_processor.rs
git commit -m "feat(video_processor): blit_into writes BGRA→NV12 directly to external texture"
```

---

## Task 3: Add `H264Encoder::new_d3d11` constructor

Mirror the `new_cuda` pattern but with D3D11VA hwaccel.

**Files:**
- Modify: `tauri-client/src-tauri/src/media/encoder.rs`

- [ ] **Step 1: Add the new constructor**

In `encoder.rs`, find `new_cuda` (around line 890). Add a sibling method `new_d3d11`:

```rust
/// Create a Windows-only D3D11 zero-copy encoder. Frames must come from
/// hw_frames_ctx-allocated D3D11 textures (use acquire_pool_frame /
/// encode_d3d11_frame). Mirrors new_cuda's pattern but with D3D11VA hwaccel.
#[cfg(target_os = "windows")]
pub fn new_d3d11(
    target_codec: crate::media::caps::CodecKind,
    config: &EncoderConfig,
    shared_device: *mut std::ffi::c_void, // ID3D11Device, transmuted via .as_raw()
) -> Result<Self, String> {
    use ffmpeg_next::sys::*;

    ffmpeg_next::init().map_err(|e| format!("FFmpeg init: {}", e))?;

    let (codec, codec_name) = Self::find_hw_encoder(target_codec)?;
    if !codec_name.contains("nvenc") {
        return Err(format!(
            "new_d3d11 only supports NVENC encoders, got '{}'",
            codec_name
        ));
    }

    // ── Build hw_device_ctx around the shared D3D11 device ─────────────
    let hw_device_ref = unsafe {
        let r = av_hwdevice_ctx_alloc(AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
        if r.is_null() {
            return Err("av_hwdevice_ctx_alloc(D3D11VA) failed".into());
        }
        // Populate the device pointer manually — same trick as new_cuda
        // does for CUcontext. AVD3D11VADeviceContext.device is the first
        // field, so the cast lands on it.
        let hw_dev_ctx = (*r).data as *mut AVHWDeviceContext;
        let d3d_ctx = (*hw_dev_ctx).hwctx as *mut std::ffi::c_void;
        *(d3d_ctx as *mut *mut std::ffi::c_void) = shared_device;

        let rc = av_hwdevice_ctx_init(r);
        if rc < 0 {
            let mut rr = r;
            av_buffer_unref(&mut rr);
            return Err(format!("av_hwdevice_ctx_init(D3D11VA) failed: {}", rc));
        }
        eprintln!("[encoder] D3D11VA hw_device_ctx initialized with shared device");
        r
    };

    // ── Build hw_frames_ctx (NV12 D3D11 texture pool, size 6) ──────────
    let hw_frames_ref = unsafe {
        let r = av_hwframe_ctx_alloc(hw_device_ref);
        if r.is_null() {
            let mut hd = hw_device_ref;
            av_buffer_unref(&mut hd);
            return Err("av_hwframe_ctx_alloc(D3D11VA) failed".into());
        }
        let frames_ctx = (*r).data as *mut AVHWFramesContext;
        (*frames_ctx).format = AVPixelFormat::AV_PIX_FMT_D3D11;
        (*frames_ctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
        (*frames_ctx).width = config.width as i32;
        (*frames_ctx).height = config.height as i32;
        (*frames_ctx).initial_pool_size = 6; // see spec §5

        let rc = av_hwframe_ctx_init(r);
        if rc < 0 {
            let mut rr = r;
            av_buffer_unref(&mut rr);
            let mut hd = hw_device_ref;
            av_buffer_unref(&mut hd);
            return Err(format!("av_hwframe_ctx_init(D3D11VA) failed: {}", rc));
        }
        eprintln!("[encoder] D3D11VA hw_frames_ctx initialized ({}x{}, pool=6)",
            config.width, config.height);
        r
    };

    // ── Build encoder context ──────────────────────────────────────────
    let mut context = ffmpeg_next::codec::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|e| format!("Encoder context: {}", e))?;

    context.set_width(config.width);
    context.set_height(config.height);
    context.set_frame_rate(Some(ffmpeg_next::Rational::new(config.fps as i32, 1)));
    context.set_time_base(ffmpeg_next::Rational::new(1, config.fps as i32));
    context.set_bit_rate((config.bitrate_kbps as usize) * 1000);
    context.set_max_bit_rate((config.bitrate_kbps as usize) * 1000);
    context.set_gop(config.fps * config.keyframe_interval_secs);
    context.set_max_b_frames(0);

    // Pixel format is D3D11 (hardware), software-format is NV12.
    unsafe {
        let ctx_ptr = context.as_mut_ptr();
        (*ctx_ptr).pix_fmt = AVPixelFormat::AV_PIX_FMT_D3D11;
        (*ctx_ptr).hw_device_ctx = av_buffer_ref(hw_device_ref);
        (*ctx_ptr).hw_frames_ctx = av_buffer_ref(hw_frames_ref);
        // VBV buffer: ~4 frames of headroom for rate control
        let vbv_bits = (config.bitrate_kbps as i32) * 1000 / (config.fps as i32) * 4;
        (*ctx_ptr).rc_buffer_size = vbv_bits;
    }

    context.set_colorspace(ffmpeg_next::color::Space::BT709);
    context.set_color_range(ffmpeg_next::color::Range::MPEG);

    // Same options dictionary as the existing CPU NVENC path.
    let mut opts = ffmpeg_next::Dictionary::new();
    opts.set("forced_idr", "1");
    opts.set("preset", "p5");
    opts.set("rc", "cbr");
    if codec_name == "hevc_nvenc" {
        opts.set("tune", "ll");
        opts.set("slices", "1");
    } else {
        opts.set("tune", "ull");
    }

    let encoder = context
        .open_with(opts)
        .map_err(|e| {
            // Clean up if open fails
            unsafe {
                let mut hd = hw_device_ref;
                av_buffer_unref(&mut hd);
                let mut hf = hw_frames_ref;
                av_buffer_unref(&mut hf);
            }
            format!("Open D3D11 encoder ({}): {}", codec_name, e)
        })?;

    eprintln!(
        "[encoder] D3D11 zero-copy encoder opened: {} — {}x{} @ {}fps, {}kbps",
        codec_name, config.width, config.height, config.fps, config.bitrate_kbps
    );

    let nv12_frame = ffmpeg_next::frame::Video::new(
        ffmpeg_next::format::Pixel::NV12, config.width, config.height,
    );

    Ok(H264Encoder {
        codec: target_codec,
        encoder,
        frame_count: 0,
        keyframe_interval: (config.fps * config.keyframe_interval_secs) as u64,
        force_next_keyframe: false,
        target_width: config.width,
        target_height: config.height,
        nv12_frame,
        scaler: None,
        supports_bgra_input: false,
        bgra_frame: None,
        bgra_scaler: None,
        #[cfg(target_os = "linux")]
        cuda_hw_device_ref: std::ptr::null_mut(),
        #[cfg(target_os = "linux")]
        cuda_hw_frames_ref: std::ptr::null_mut(),
        #[cfg(target_os = "linux")]
        is_vaapi_hw: false,
    })
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check
```

Expected: compiles. The new function is unused so far.

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/media/encoder.rs
git commit -m "feat(encoder): new_d3d11 constructor — NVENC via FFmpeg D3D11VA hwaccel"
```

---

## Task 4: Add `acquire_pool_frame` and `encode_d3d11_frame` methods

These are the per-frame entry points the pipeline calls. `acquire_pool_frame` returns an `AVFrame` whose `data[0]` is a pointer to a pool-managed `ID3D11Texture2D`; the caller blits BGRA→NV12 into that texture, then submits via `encode_d3d11_frame`.

**Files:**
- Modify: `tauri-client/src-tauri/src/media/encoder.rs`

- [ ] **Step 1: Add a `D3d11PoolFrame` wrapper that exposes the texture**

Right after `new_d3d11`, add:

```rust
/// Wraps an AVFrame allocated from the D3D11 hw_frames_ctx pool. Holds
/// the raw AVFrame pointer because the FFmpeg-next typed `frame::Video`
/// doesn't expose the data[0] pointer in the way we need (D3D11 texture
/// pointer rather than raw image bytes).
#[cfg(target_os = "windows")]
pub struct D3d11PoolFrame {
    av_frame: *mut ffmpeg_next::sys::AVFrame,
}

#[cfg(target_os = "windows")]
impl D3d11PoolFrame {
    /// Get the D3D11 texture this pool frame wraps. The VideoProcessor
    /// blits BGRA→NV12 directly into this texture.
    pub fn texture(&self) -> windows::Win32::Graphics::Direct3D11::ID3D11Texture2D {
        use windows::core::Interface;
        unsafe {
            // FFmpeg's D3D11VA convention: frame->data[0] is the
            // ID3D11Texture2D*, frame->data[1] is the array slice index
            // (intptr_t). The pool gives us texture array slice 0 typically.
            let raw_tex = (*self.av_frame).data[0] as *mut std::ffi::c_void;
            // SAFETY: the raw pointer is an ID3D11Texture2D handed to us by
            // FFmpeg's hw_frames_ctx pool. AddRef so our wrapper keeps it
            // alive independently; FFmpeg releases its own ref when the
            // AVFrame is freed.
            let tex_ptr = raw_tex as *mut std::ffi::c_void;
            let tex: ID3D11Texture2D = ID3D11Texture2D::from_raw(tex_ptr);
            // The from_raw above takes ownership; we want to keep FFmpeg's
            // reference too. AddRef and forget the owned wrapper, then
            // re-create with a fresh AddRef so the caller can own it.
            std::mem::forget(tex.clone());
            tex
        }
    }

    /// The array-slice index for the D3D11 texture (some FFmpeg builds
    /// share one texture array across the pool with per-slice access;
    /// the pipeline doesn't usually need this directly, but it's available).
    pub fn array_slice(&self) -> usize {
        unsafe { (*self.av_frame).data[1] as usize }
    }
}

#[cfg(target_os = "windows")]
impl Drop for D3d11PoolFrame {
    fn drop(&mut self) {
        unsafe {
            ffmpeg_next::sys::av_frame_free(&mut self.av_frame);
        }
    }
}
```

(The `Interface` trait in `windows::core` provides `from_raw`. `ID3D11Texture2D::clone()` increments the COM refcount.)

- [ ] **Step 2: Add `acquire_pool_frame` on `H264Encoder`**

In the same `impl H264Encoder` block:

```rust
/// Allocate a D3D11 NV12 texture from the encoder's hw_frames_ctx pool.
/// Caller blits into the returned frame's texture, then submits via
/// encode_d3d11_frame.
#[cfg(target_os = "windows")]
pub fn acquire_pool_frame(&mut self) -> Result<D3d11PoolFrame, String> {
    use ffmpeg_next::sys::*;
    unsafe {
        let av_frame = av_frame_alloc();
        if av_frame.is_null() {
            return Err("av_frame_alloc failed".into());
        }
        // Read the encoder's hw_frames_ctx (we set it in new_d3d11).
        let ctx_ptr = self.encoder.as_ptr();
        let frames_ref = (*ctx_ptr).hw_frames_ctx;
        if frames_ref.is_null() {
            av_frame_free(&mut (av_frame as *mut _));
            return Err("encoder has no hw_frames_ctx — was it built via new_d3d11?".into());
        }
        let rc = av_hwframe_get_buffer(frames_ref, av_frame, 0);
        if rc < 0 {
            av_frame_free(&mut (av_frame as *mut _));
            return Err(format!("av_hwframe_get_buffer: {}", rc));
        }
        Ok(D3d11PoolFrame { av_frame })
    }
}
```

- [ ] **Step 3: Add `encode_d3d11_frame` on `H264Encoder`**

```rust
/// Submit a pool frame (already filled by VideoProcessor) to the encoder.
/// Returns the next encoded packet if one is ready, or None if the encoder
/// is buffering.
#[cfg(target_os = "windows")]
pub fn encode_d3d11_frame(
    &mut self,
    pool_frame: D3d11PoolFrame,
) -> Result<Option<EncodedFrame>, String> {
    use ffmpeg_next::sys::*;
    unsafe {
        // Set pts on the AVFrame.
        let pts = self.frame_count as i64;
        (*pool_frame.av_frame).pts = pts;

        // Force keyframe if requested (responds to PLI).
        if self.force_next_keyframe {
            (*pool_frame.av_frame).pict_type = AVPictureType::AV_PICTURE_TYPE_I;
            (*pool_frame.av_frame).flags |= AV_FRAME_FLAG_KEY as i32;
            self.force_next_keyframe = false;
        } else {
            (*pool_frame.av_frame).pict_type = AVPictureType::AV_PICTURE_TYPE_NONE;
        }

        // Submit. ffmpeg-next doesn't have a public typed wrapper for
        // sending a raw AVFrame, so use the sys API directly.
        let ctx_ptr = self.encoder.as_mut_ptr();
        let rc = avcodec_send_frame(ctx_ptr, pool_frame.av_frame);
        if rc < 0 && rc != AVERROR(EAGAIN) {
            return Err(format!("avcodec_send_frame: {}", rc));
        }
        // pool_frame is dropped here, releasing the texture back to the pool
        // (FFmpeg internally retains a ref while encoding).
        drop(pool_frame);
    }

    self.frame_count += 1;
    // Drain the encoder via the existing helper (handles AVCC packing,
    // description extraction per codec, etc.)
    Ok(self.receive_one_packet())
}
```

- [ ] **Step 4: Verify it compiles**

```bash
cargo check
```

Expected: compiles. May need to add `EAGAIN` to the `use ffmpeg_next::sys::*;` import if not auto-imported.

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src-tauri/src/media/encoder.rs
git commit -m "feat(encoder): acquire_pool_frame + encode_d3d11_frame for D3D11 zero-copy submission"
```

---

## Task 5: Define `GpuFrame` + `GpuCaptureSource` trait + shared device helper

**Files:**
- Create: `tauri-client/src-tauri/src/media/gpu_capture.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

- [ ] **Step 1: Add the module declaration**

In `tauri-client/src-tauri/src/media/mod.rs`, after the `video_processor` line:

```rust
#[cfg(target_os = "windows")]
pub mod gpu_capture;
```

- [ ] **Step 2: Write the module**

Create `tauri-client/src-tauri/src/media/gpu_capture.rs`:

```rust
//! Trait + types shared by the Windows GPU capture sources (DXGI Desktop
//! Duplication and Windows Graphics Capture). Both produce BGRA D3D11
//! textures into the GpuStreamingPipeline's shared device.

#![cfg(target_os = "windows")]

use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

#[derive(Debug)]
pub struct GpuFrame {
    /// BGRA texture in the shared device's VRAM. Caller must release this
    /// (via the source's release_current_frame for DXGI; WGC manages
    /// implicit lifetime via the FramePool drop).
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
}

#[derive(Debug)]
pub enum CaptureError {
    Timeout,        // no new frame ready; caller should poll again
    AccessLost,     // device removed / output reconfigured
    Disconnected,   // source ended (window closed, monitor unplugged)
    Other(String),
}

pub trait GpuCaptureSource: Send {
    /// Native width of the source (monitor or window) in luma pixels.
    /// Used by the VideoProcessor to size the input view; may differ
    /// from the encoder's target width when the user requests scaling.
    fn width(&self) -> u32;

    /// Native height of the source. See width().
    fn height(&self) -> u32;

    /// Returns the next available frame, or Ok(None) on timeout. The
    /// returned GpuFrame is valid until release_current_frame() is called.
    fn next_frame(&mut self) -> Result<Option<GpuFrame>, CaptureError>;

    /// Release the most recently acquired frame back to the source.
    /// Required for DXGI (one frame in flight at a time); no-op for WGC.
    fn release_current_frame(&mut self);
}
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo check
```

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/gpu_capture.rs \
        tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(gpu_capture): GpuFrame + GpuCaptureSource trait shared by DXGI and WGC"
```

---

## Task 6: Implement `DxgiSource`

A new GPU capture source in `capture_dxgi.rs` that uses an external `ID3D11Device` and produces BGRA textures via the trait. The existing `start_capture` (CPU readback path) stays untouched.

**Files:**
- Modify: `tauri-client/src-tauri/src/media/capture_dxgi.rs`

- [ ] **Step 1: Add the struct + constructor**

At the bottom of `capture_dxgi.rs`, add:

```rust
use super::gpu_capture::{CaptureError, GpuCaptureSource, GpuFrame};

pub struct DxgiSource {
    duplication: IDXGIOutputDuplication,
    width: u32,
    height: u32,
    start_time: std::time::Instant,
    frame_acquired: bool,
}

impl DxgiSource {
    /// Create a DXGI source for the given monitor index, using the supplied
    /// shared D3D11 device. The device must be on the same adapter as the
    /// target output (use `create_device_for_adapter` to ensure this).
    pub fn new(
        adapter_idx: u32,
        output_idx: u32,
        device: &ID3D11Device,
    ) -> Result<Self, String> {
        unsafe {
            let factory: IDXGIFactory1 =
                CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {}", e))?;
            let adapter: IDXGIAdapter1 = factory
                .EnumAdapters1(adapter_idx)
                .map_err(|e| format!("EnumAdapters1({}): {}", adapter_idx, e))?;
            let output: IDXGIOutput = adapter
                .EnumOutputs(output_idx)
                .map_err(|e| format!("EnumOutputs({}): {}", output_idx, e))?;
            let output1: IDXGIOutput1 = output
                .cast()
                .map_err(|e| format!("Cast IDXGIOutput1: {}", e))?;

            let desc = output.GetDesc().map_err(|e| format!("GetDesc: {}", e))?;
            let coords = desc.DesktopCoordinates;
            let width = (coords.right - coords.left).unsigned_abs();
            let height = (coords.bottom - coords.top).unsigned_abs();

            let duplication: IDXGIOutputDuplication = output1
                .DuplicateOutput(device)
                .map_err(|e| format!("DuplicateOutput: {}", e))?;

            Ok(DxgiSource {
                duplication,
                width,
                height,
                start_time: std::time::Instant::now(),
                frame_acquired: false,
            })
        }
    }

    pub fn width(&self) -> u32 { self.width }
    pub fn height(&self) -> u32 { self.height }
}

impl GpuCaptureSource for DxgiSource {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }

    fn next_frame(&mut self) -> Result<Option<GpuFrame>, CaptureError> {
        if self.frame_acquired {
            return Err(CaptureError::Other(
                "Previous frame not released — call release_current_frame first".into(),
            ));
        }
        unsafe {
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut desktop_resource: Option<IDXGIResource> = None;
            let hr = self.duplication.AcquireNextFrame(
                16, // ms timeout — ~one frame at 60fps
                &mut frame_info,
                &mut desktop_resource,
            );
            match hr {
                Ok(()) => {}
                Err(e) => {
                    let code = e.code().0 as u32;
                    if code == 0x887A0027 { return Ok(None); }   // WAIT_TIMEOUT
                    if code == 0x887A0026 { return Err(CaptureError::AccessLost); }
                    return Err(CaptureError::Other(format!("AcquireNextFrame: {}", e)));
                }
            }
            let resource = match desktop_resource {
                Some(r) => r,
                None => {
                    let _ = self.duplication.ReleaseFrame();
                    return Ok(None);
                }
            };
            let texture: ID3D11Texture2D = resource
                .cast()
                .map_err(|e| CaptureError::Other(format!("Cast IDXGIResource→ID3D11Texture2D: {}", e)))?;
            self.frame_acquired = true;
            Ok(Some(GpuFrame {
                texture,
                width: self.width,
                height: self.height,
                timestamp_us: self.start_time.elapsed().as_micros() as u64,
            }))
        }
    }

    fn release_current_frame(&mut self) {
        if self.frame_acquired {
            unsafe { let _ = self.duplication.ReleaseFrame(); }
            self.frame_acquired = false;
        }
    }
}

impl Drop for DxgiSource {
    fn drop(&mut self) { self.release_current_frame(); }
}
```

- [ ] **Step 2: Verify it compiles**

```bash
cargo check
```

Expected: compiles. The struct is unused so far.

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/media/capture_dxgi.rs
git commit -m "feat(capture_dxgi): DxgiSource implementing GpuCaptureSource"
```

---

## Task 7: Implement `WgcSource`

Same pattern for window capture. Reuses the existing `create_winrt_device` to wrap our shared device as `IDirect3DDevice` for WGC's `Direct3D11CaptureFramePool`.

**Files:**
- Modify: `tauri-client/src-tauri/src/media/capture_wgc.rs`

- [ ] **Step 1: Find existing helpers**

In `capture_wgc.rs`, you should already have:
- `fn create_winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice, String>` (around line 356)
- A `start_capture` function that does CPU readback (keep it, untouched)

Read both to confirm their signatures match what we'll call below.

- [ ] **Step 2: Promote `create_winrt_device` to public**

Change `fn create_winrt_device(...)` to `pub(crate) fn create_winrt_device(...)`.

- [ ] **Step 3: Add `WgcSource` struct + impl**

At the bottom of `capture_wgc.rs`, add:

```rust
use super::gpu_capture::{CaptureError, GpuCaptureSource, GpuFrame};
use windows::Graphics::Capture::{Direct3D11CaptureFramePool, GraphicsCaptureItem};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;

pub struct WgcSource {
    item: GraphicsCaptureItem,
    frame_pool: Direct3D11CaptureFramePool,
    session: windows::Graphics::Capture::GraphicsCaptureSession,
    width: u32,
    height: u32,
    start_time: std::time::Instant,
}

impl WgcSource {
    /// Create a WGC source for the given window HWND, using the supplied
    /// shared D3D11 device. WGC will hand out frames in the same device's VRAM.
    pub fn new(hwnd: windows::Win32::Foundation::HWND, device: &ID3D11Device) -> Result<Self, String> {
        // Wrap the shared D3D11 device as IDirect3DDevice for WinRT.
        let winrt_device = create_winrt_device(device)?;

        // Create the capture item from the HWND.
        let interop = windows::core::factory::<
            GraphicsCaptureItem,
            windows::Graphics::Capture::IGraphicsCaptureItemInterop,
        >()
        .map_err(|e| format!("GraphicsCaptureItemInterop factory: {}", e))?;
        let item: GraphicsCaptureItem = unsafe {
            interop
                .CreateForWindow(hwnd)
                .map_err(|e| format!("CreateForWindow: {}", e))?
        };
        let size = item.Size().map_err(|e| format!("Size: {}", e))?;
        let width = size.Width as u32;
        let height = size.Height as u32;

        // Build the frame pool (free-threaded so we can poll from any thread).
        let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2, // num buffers
            size,
        )
        .map_err(|e| format!("CreateFreeThreaded: {}", e))?;

        let session = frame_pool
            .CreateCaptureSession(&item)
            .map_err(|e| format!("CreateCaptureSession: {}", e))?;
        // Hide the yellow capture border (most recent Win11 builds support this).
        let _ = session.SetIsBorderRequired(false);
        // Hide the cursor or not — keep default (visible) to match existing behavior.
        session.StartCapture().map_err(|e| format!("StartCapture: {}", e))?;

        Ok(WgcSource {
            item,
            frame_pool,
            session,
            width,
            height,
            start_time: std::time::Instant::now(),
        })
    }

    pub fn width(&self) -> u32 { self.width }
    pub fn height(&self) -> u32 { self.height }
}

impl GpuCaptureSource for WgcSource {
    fn width(&self) -> u32 { self.width }
    fn height(&self) -> u32 { self.height }

    fn next_frame(&mut self) -> Result<Option<GpuFrame>, CaptureError> {
        let frame = match self.frame_pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(e) => return Err(CaptureError::Other(format!("TryGetNextFrame: {}", e))),
        };
        // TryGetNextFrame returns null-equivalent (Err in Rust bindings) when
        // there's no new frame; some bindings give an empty handle. The
        // FrameArrived event would be cleaner but polling is fine for our
        // single-thread loop.
        let surface = frame.Surface().map_err(|e| CaptureError::Other(format!("Surface: {}", e)))?;
        let interface_access: IDirect3DDxgiInterfaceAccess = surface
            .cast()
            .map_err(|e| CaptureError::Other(format!("Cast IDirect3DDxgiInterfaceAccess: {}", e)))?;
        let texture: ID3D11Texture2D = unsafe {
            interface_access
                .GetInterface()
                .map_err(|e| CaptureError::Other(format!("GetInterface ID3D11Texture2D: {}", e)))?
        };
        // The Direct3D11CaptureFrame is dropped at end of this function;
        // its texture remains valid through the AddRef inside cast/GetInterface.
        Ok(Some(GpuFrame {
            texture,
            width: self.width,
            height: self.height,
            timestamp_us: self.start_time.elapsed().as_micros() as u64,
        }))
    }

    fn release_current_frame(&mut self) { /* WGC manages frame lifetime */ }
}

impl Drop for WgcSource {
    fn drop(&mut self) {
        let _ = self.session.Close();
        let _ = self.frame_pool.Close();
    }
}
```

- [ ] **Step 4: Verify it compiles**

```bash
cargo check
```

Expected: compiles. May surface a missing windows-feature for `Graphics_Capture` — should already be there since `start_capture` uses WGC; if not, add it.

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src-tauri/src/media/capture_wgc.rs
git commit -m "feat(capture_wgc): WgcSource implementing GpuCaptureSource"
```

---

## Task 8: Build `GpuStreamingPipeline` orchestrator

The single-thread loop that pulls frames, blits, encodes, and emits packets/events.

**Files:**
- Create: `tauri-client/src-tauri/src/media/gpu_pipeline.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

- [ ] **Step 1: Add module declaration**

In `tauri-client/src-tauri/src/media/mod.rs`, after `gpu_capture`:

```rust
#[cfg(target_os = "windows")]
pub mod gpu_pipeline;
```

- [ ] **Step 2: Write the orchestrator**

Create `tauri-client/src-tauri/src/media/gpu_pipeline.rs`:

```rust
//! GPU-only sender pipeline for Windows: capture → BGRA→NV12 → NVENC.
//! No GPU↔CPU readback. Single thread runs the whole loop.

#![cfg(target_os = "windows")]

use std::sync::Arc;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11DeviceContext};

use super::caps::CodecKind;
use super::encoder::{EncoderConfig, H264Encoder};
use super::gpu_capture::{CaptureError, GpuCaptureSource};
use super::video_processor::VideoProcessor;
use super::capture_dxgi;

pub struct GpuStreamingPipeline {
    device: Arc<ID3D11Device>,
    context: ID3D11DeviceContext,
    capture: Box<dyn GpuCaptureSource>,
    converter: VideoProcessor,
    encoder: H264Encoder,
}

pub struct GpuPipelineCallbacks {
    /// Called for each encoded packet ready to ship.
    pub on_encoded: Box<dyn FnMut(super::encoder::EncodedFrame) + Send>,
    /// Called when the pipeline exits unexpectedly.
    pub on_error: Box<dyn FnOnce(String) + Send>,
}

impl GpuStreamingPipeline {
    /// Build the pipeline. Returns Err if any D3D11 / NVENC step fails;
    /// caller should fall back to the CPU path on Err.
    pub fn build(
        target_codec: CodecKind,
        source_id: &str,
        config: EncoderConfig,
    ) -> Result<Self, String> {
        // Source IDs are formatted by capture::list_sources:
        //   "monitor:{adapter_idx}:{output_idx}"  → DxgiSource
        //   "window:{hwnd}"                       → WgcSource
        let (capture, device_ctx, device): (Box<dyn GpuCaptureSource>, _, _) =
            if let Some(rest) = source_id.strip_prefix("monitor:") {
                let mut parts = rest.splitn(2, ':');
                let adapter_idx: u32 = parts
                    .next().and_then(|s| s.parse().ok())
                    .ok_or("monitor source ID malformed (adapter_idx)")?;
                let output_idx: u32 = parts
                    .next().and_then(|s| s.parse().ok())
                    .ok_or("monitor source ID malformed (output_idx)")?;

                // Build the shared device on the same adapter as the output.
                use windows::core::Interface;
                use windows::Win32::Graphics::Dxgi::*;
                let factory: IDXGIFactory1 = unsafe {
                    CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {}", e))?
                };
                let adapter: IDXGIAdapter1 = unsafe {
                    factory.EnumAdapters1(adapter_idx)
                        .map_err(|e| format!("EnumAdapters1: {}", e))?
                };
                let (device, context) = capture_dxgi::create_device_for_adapter(&adapter)?;
                let src = capture_dxgi::DxgiSource::new(adapter_idx, output_idx, &device)?;
                (Box::new(src), context, device)
            } else if let Some(_hwnd_str) = source_id.strip_prefix("window:") {
                // Reuse the WGC source; build a generic device (any adapter).
                let hwnd_str = source_id.strip_prefix("window:").unwrap();
                let hwnd_raw: usize = hwnd_str.parse().map_err(|_| "window source ID malformed")?;
                let hwnd = windows::Win32::Foundation::HWND(hwnd_raw as *mut std::ffi::c_void);

                use windows::Win32::Graphics::Direct3D::*;
                let mut device: Option<ID3D11Device> = None;
                let mut context: Option<ID3D11DeviceContext> = None;
                let mut actual_level = D3D_FEATURE_LEVEL_11_0;
                unsafe {
                    windows::Win32::Graphics::Direct3D11::D3D11CreateDevice(
                        None,
                        D3D_DRIVER_TYPE_HARDWARE,
                        windows::Win32::Foundation::HMODULE::default(),
                        windows::Win32::Graphics::Direct3D11::D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                        Some(&[D3D_FEATURE_LEVEL_11_0]),
                        windows::Win32::Graphics::Direct3D11::D3D11_SDK_VERSION,
                        Some(&mut device),
                        Some(&mut actual_level),
                        Some(&mut context),
                    )
                    .map_err(|e| format!("D3D11CreateDevice: {}", e))?;
                }
                let device = device.ok_or("D3D11CreateDevice returned None")?;
                let context = context.ok_or("D3D11CreateDevice context None")?;
                let src = super::capture_wgc::WgcSource::new(hwnd, &device)?;
                (Box::new(src), context, device)
            } else {
                return Err(format!("Unknown source ID prefix: {}", source_id));
            };

        // Source native dims for the VideoProcessor input view; may differ
        // from encoder target dims when the user requests scaling.
        let src_w = capture.width();
        let src_h = capture.height();

        // GPU color converter (BGRA src_w×src_h → NV12 config.width×config.height).
        // Same blit handles both color conversion and scaling in one op.
        let converter = VideoProcessor::new(&device, src_w, src_h, config.width, config.height)
            .map_err(|e| format!("VideoProcessor::new: {}", e))?;

        // Encoder via D3D11VA hwaccel.
        use windows::core::Interface;
        let device_raw = device.as_raw() as *mut std::ffi::c_void;
        let encoder = H264Encoder::new_d3d11(target_codec, &config, device_raw)
            .map_err(|e| format!("H264Encoder::new_d3d11: {}", e))?;

        Ok(GpuStreamingPipeline {
            device: Arc::new(device),
            context,
            capture,
            converter,
            encoder,
        })
    }

    /// Run the capture→convert→encode loop on the calling thread.
    /// Returns when shutdown is requested (via the shutdown_flag) or on
    /// fatal error.
    pub fn run(
        mut self,
        shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
        mut callbacks: GpuPipelineCallbacks,
    ) {
        use std::sync::atomic::Ordering;
        loop {
            if shutdown_flag.load(Ordering::Relaxed) { break; }

            let frame = match self.capture.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => continue, // timeout, no new frame
                Err(CaptureError::AccessLost) => {
                    (callbacks.on_error)("DXGI access lost".into());
                    return;
                }
                Err(CaptureError::Disconnected) => {
                    eprintln!("[gpu-pipeline] capture source disconnected");
                    return;
                }
                Err(CaptureError::Other(msg)) => {
                    (callbacks.on_error)(format!("Capture error: {}", msg));
                    return;
                }
                Err(CaptureError::Timeout) => continue,
            };

            // Acquire pool frame, blit BGRA→NV12 directly into it, submit.
            let pool_frame = match self.encoder.acquire_pool_frame() {
                Ok(f) => f,
                Err(e) => {
                    self.capture.release_current_frame();
                    (callbacks.on_error)(format!("acquire_pool_frame: {}", e));
                    return;
                }
            };
            let nv12_tex = pool_frame.texture();
            if let Err(e) = self.converter.blit_into(&self.context, &frame.texture, &nv12_tex) {
                self.capture.release_current_frame();
                (callbacks.on_error)(format!("blit_into: {}", e));
                return;
            }
            // Drop the texture handle we cloned out of the pool frame
            // BEFORE submitting — submit consumes the pool frame.
            drop(nv12_tex);

            self.capture.release_current_frame();

            match self.encoder.encode_d3d11_frame(pool_frame) {
                Ok(Some(encoded)) => (callbacks.on_encoded)(encoded),
                Ok(None) => {} // encoder buffering
                Err(e) => {
                    (callbacks.on_error)(format!("encode_d3d11_frame: {}", e));
                    return;
                }
            }
        }
        eprintln!("[gpu-pipeline] shutdown");
    }
}
```

- [ ] **Step 3: Verify it compiles**

```bash
cargo check
```

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/gpu_pipeline.rs \
        tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(gpu_pipeline): GpuStreamingPipeline orchestrator with single-thread loop"
```

---

## Task 9: Add `STREAM_GPU_FALLBACK` event

**Files:**
- Modify: `tauri-client/src-tauri/src/events/mod.rs`

- [ ] **Step 1: Add the event constant + payload + emitter**

In `tauri-client/src-tauri/src/events/mod.rs`, alongside the existing `STREAM_CODEC_CHANGED`:

```rust
// Plan-D-1: emitted when the GPU zero-copy pipeline fails to start
// and we fall back to the CPU readback path.
pub const STREAM_GPU_FALLBACK: &str = "stream_gpu_fallback";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamGpuFallbackPayload {
    pub error: String,
}

pub fn emit_stream_gpu_fallback(app: &AppHandle, error: String) {
    let _ = app.emit(STREAM_GPU_FALLBACK, StreamGpuFallbackPayload { error });
}
```

- [ ] **Step 2: Verify compiles**

```bash
cargo check
```

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/events/mod.rs
git commit -m "feat(events): STREAM_GPU_FALLBACK event for CPU-path fallback notification"
```

---

## Task 10: Wire selection logic into `start_screen_share`

**Files:**
- Modify: `tauri-client/src-tauri/src/commands/streaming.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs` (`VideoEngine::start` may need a Windows-conditional alt path)

- [ ] **Step 1: Add a "try GPU pipeline, else CPU pipeline" selector function**

In `commands/streaming.rs`, add a private helper near the top (above `start_screen_share`):

```rust
/// Plan-D-1: on Windows + hardware codec, try the new D3D11 zero-copy
/// pipeline. On any failure or non-hardware codec, fall back to the
/// existing CPU-readback pipeline and emit a fallback event.
#[cfg(target_os = "windows")]
fn try_start_gpu_pipeline(
    target_codec: crate::media::caps::CodecKind,
    source_id: &str,
    encoder_config: crate::media::encoder::EncoderConfig,
    app: &tauri::AppHandle,
    shutdown_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    on_encoded: Box<dyn FnMut(crate::media::encoder::EncodedFrame) + Send>,
) -> Result<std::thread::JoinHandle<()>, String> {
    use crate::media::caps::CodecKind;
    if !matches!(target_codec, CodecKind::H264Hw | CodecKind::H265 | CodecKind::Av1) {
        return Err("software codec — use CPU path".to_string());
    }
    let pipeline = crate::media::gpu_pipeline::GpuStreamingPipeline::build(
        target_codec, source_id, encoder_config,
    )?;
    let app_clone = app.clone();
    let handle = std::thread::Builder::new()
        .name("decibell-gpu-pipeline".to_string())
        .spawn(move || {
            pipeline.run(
                shutdown_flag,
                crate::media::gpu_pipeline::GpuPipelineCallbacks {
                    on_encoded,
                    on_error: Box::new(move |e| {
                        eprintln!("[gpu-pipeline] error: {}", e);
                        crate::events::emit_stream_gpu_fallback(&app_clone, e);
                    }),
                },
            );
        })
        .map_err(|e| format!("spawn gpu pipeline thread: {}", e))?;
    Ok(handle)
}
```

- [ ] **Step 2: Plumb the selection in `start_screen_share`**

In `start_screen_share`, find where `VideoEngine::start` is called (around line 125). BEFORE that call, attempt the GPU pipeline:

```rust
// Plan-D-1: prefer GPU zero-copy pipeline on Windows for hardware codecs.
// On any failure, fall back to the existing VideoEngine::start (CPU path)
// and emit a toast event so the user knows.
#[cfg(target_os = "windows")]
{
    let on_encoded: Box<dyn FnMut(crate::media::encoder::EncodedFrame) + Send> = {
        // Same packetization path as VideoEngine — the encoded bytes need
        // to be split into UdpVideoPackets and sent. Reuse the existing
        // packetization helper or send via a channel into VideoEngine's
        // existing packetizer.
        // For now: log only — wiring the encoded frames into the existing
        // UDP send pipeline is a follow-up step in this same task.
        Box::new(|_frame| {
            // TODO: wire to UdpVideoPacket packetization + send
        })
    };
    let shutdown_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    match try_start_gpu_pipeline(
        target_codec, &source_id, encoder_config.clone(), &app, shutdown_flag.clone(), on_encoded,
    ) {
        Ok(_handle) => {
            // GPU pipeline running. Skip the CPU VideoEngine::start below.
            // (Wire shutdown_flag into AppState so leave_voice_channel triggers it.)
            eprintln!("[stream] GPU zero-copy pipeline started for {:?}", target_codec);
            // ... store handle, store shutdown_flag in AppState somehow
            return Ok(());
        }
        Err(e) => {
            eprintln!("[stream] GPU pipeline unavailable, falling back: {}", e);
            crate::events::emit_stream_gpu_fallback(&app, e);
            // Fall through to CPU path below.
        }
    }
}

// Existing CPU path follows...
```

> **Implementer note:** the comment "TODO: wire to UdpVideoPacket packetization + send" inside the `on_encoded` closure is a real implementation gap that this task must close. The gap is too large for a code skeleton — the existing packetization is inside `video_pipeline::run_video_send_pipeline` (search for `UdpVideoPacket::new_with_codec`). Extract the per-frame packetization into a small public helper function in `video_pipeline.rs` like `send_encoded_frame_as_packets(socket: &UdpSocket, sender_id: &str, frame_id: u32, encoded: &EncodedFrame, codec_byte: u8)`. The GPU pipeline's `on_encoded` callback can then call that helper. Same logic, same wire format, just reused.
>
> Also: `pipeline_thread` lifecycle and `shutdown_flag` need to be stored in `AppState` so `stop_screen_share` can signal it. Mirror how the existing `VideoEngine` is stored.

- [ ] **Step 3: Refactor packetization helper out of `video_pipeline.rs`**

In `tauri-client/src-tauri/src/media/video_pipeline.rs`, find the inner block where `UdpVideoPacket::new_with_codec` is called per frame chunk (search `chunks.iter().enumerate()`). Extract into a public function:

```rust
/// Public helper: split an EncodedFrame into UdpVideoPacket fragments and
/// send them on the given UDP socket. Used by both the CPU pipeline
/// (existing run_video_send_pipeline loop) and the GPU pipeline
/// (gpu_pipeline.rs callback).
pub fn send_encoded_frame_as_packets(
    socket: &std::net::UdpSocket,
    sender_id: &str,
    frame_id: u32,
    encoded: &super::encoder::EncodedFrame,
    codec_byte: u8,
) -> (u32, u32) {
    use super::video_packet::{UdpVideoPacket, UDP_MAX_PAYLOAD};
    let chunks: Vec<&[u8]> = encoded.data.chunks(UDP_MAX_PAYLOAD).collect();
    let total = chunks.len() as u16;
    let mut send_ok = 0u32;
    let mut send_err = 0u32;
    for (i, chunk) in chunks.iter().enumerate() {
        let pkt = UdpVideoPacket::new_with_codec(
            sender_id,
            frame_id,
            i as u16,
            total,
            encoded.is_keyframe,
            codec_byte,
            chunk,
        );
        match socket.send(&pkt.to_bytes()) {
            Ok(_) => send_ok += 1,
            Err(_) => send_err += 1,
        }
    }
    (send_ok, send_err)
}
```

In the existing `run_video_send_pipeline`, replace the inner chunk-send loop with a call to this helper.

- [ ] **Step 4: Wire `send_encoded_frame_as_packets` into the GPU pipeline `on_encoded` callback**

Back in `commands/streaming.rs`'s `try_start_gpu_pipeline` site, replace the TODO closure with:

```rust
let socket_clone = media_socket.clone();   // VoiceEngine::media_socket()
let sender_id_clone = sender_id.clone();
let codec_byte = target_codec as u8;
let mut frame_id: u32 = 0;
let on_encoded: Box<dyn FnMut(crate::media::encoder::EncodedFrame) + Send> = Box::new(
    move |frame| {
        let _ = crate::media::video_pipeline::send_encoded_frame_as_packets(
            &socket_clone,
            &sender_id_clone,
            frame_id,
            &frame,
            codec_byte,
        );
        frame_id = frame_id.wrapping_add(1);
    }
);
```

- [ ] **Step 5: Add a place to store the GPU pipeline shutdown flag in `AppState`**

In `tauri-client/src-tauri/src/state.rs`, add a field:

```rust
#[cfg(target_os = "windows")]
pub gpu_pipeline_shutdown: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
```

Initialize in `AppState::new`:

```rust
#[cfg(target_os = "windows")]
gpu_pipeline_shutdown: None,
```

In `start_screen_share`, after the GPU pipeline starts, store the flag:

```rust
{
    let mut s = state.lock().await;
    #[cfg(target_os = "windows")]
    {
        s.gpu_pipeline_shutdown = Some(shutdown_flag.clone());
    }
}
```

In `stop_screen_share` (find it in `commands/streaming.rs`), trigger shutdown:

```rust
{
    let mut s = state.lock().await;
    #[cfg(target_os = "windows")]
    if let Some(flag) = s.gpu_pipeline_shutdown.take() {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // ... existing CPU pipeline stop logic
}
```

- [ ] **Step 6: Verify compiles + run lib tests**

```bash
cargo check
cargo test --lib
```

Both expected to pass.

- [ ] **Step 7: Commit**

```bash
git add tauri-client/src-tauri/src/commands/streaming.rs \
        tauri-client/src-tauri/src/media/video_pipeline.rs \
        tauri-client/src-tauri/src/state.rs
git commit -m "feat(streaming): wire GPU zero-copy pipeline with CPU fallback + shutdown"
```

---

## Task 11: React toast for fallback event

**Files:**
- Modify: `tauri-client/src/features/voice/useVoiceEvents.ts`

- [ ] **Step 1: Add the listener**

In `useVoiceEvents.ts`, alongside the existing `stream_codec_changed` listener, add:

```typescript
promises.push(listen<{ error: string }>("stream_gpu_fallback", async (event) => {
  const { useToastStore } = await import("../../stores/toastStore");
  useToastStore.getState().push({
    severity: "warning",
    title: "GPU encoding unavailable",
    body: `Streaming via CPU path — higher CPU usage. ${event.payload.error}`,
    duration: 6000,
  });
}));
```

- [ ] **Step 2: TypeScript check**

```bash
cd tauri-client
npx tsc --noEmit 2>&1 | grep useVoiceEvents | head -3
```

Expected: no errors.

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src/features/voice/useVoiceEvents.ts
git commit -m "feat(voice): toast notification when GPU pipeline falls back to CPU"
```

---

## Task 12: End-to-end verification

**Files:** none — measurement task.

- [ ] **Step 1: Build the dev binary**

```bash
cd tauri-client
npm run tauri dev
```

- [ ] **Step 2: Open Task Manager (Performance → CPU and GPU tabs)**

- [ ] **Step 3: Start a 1080p60 H.264 stream from the dev app, observe CPU usage of `decibell.exe`**

Expected: with the GPU pipeline active, CPU usage should be ~1–3% (just packetization + UDP). Compare against pre-change ~10–25%. The encoder log should show `[encoder] D3D11 zero-copy encoder opened: h264_nvenc — ...` and there should be NO `STREAM_GPU_FALLBACK` toast.

- [ ] **Step 4: Force fallback for testing**

Temporarily edit `gpu_pipeline.rs::build` to return `Err("forced fallback for testing".into())` at the very top. Restart the app. Stream should still start (using CPU path), and a warning toast should appear: "GPU encoding unavailable — Streaming via CPU path — higher CPU usage. forced fallback for testing".

Revert the temporary change.

- [ ] **Step 5: Verify HEVC and AV1 also use the GPU path**

Force AV1 via the codec dropdown, start a stream, confirm the encoder log shows `D3D11 zero-copy encoder opened: av1_nvenc — ...`. Same for HEVC (`hevc_nvenc`).

- [ ] **Step 6: Verify libx264 still works on CPU path**

Force H.264 (software) via the codec dropdown. Stream should start without GPU pipeline init logs and without the fallback toast — software codec deliberately skips the GPU attempt.

- [ ] **Step 7: Commit verification notes (optional)**

If you observed specific CPU% before/after, write them in a one-line `docs/superpowers/specs/2026-04-28-windows-d3d11-zero-copy-design.md` addendum at the bottom of section 8.

---

## Spec Coverage Self-Check

Cross-reference each spec section against the plan tasks:

- §1 Goal — Tasks 8, 10 deliver the zero-copy path
- §2 Scope (Windows + NVENC + DXGI + WGC) — Task 3 (`new_d3d11` errors on non-NVENC), Task 6 (DXGI), Task 7 (WGC)
- §3 Architecture diagram — Task 8 implements
- §4 D3D11 device ownership — Task 8 (`build()` constructs shared device + multi-monitor caveat handled via per-stream device build)
- §5 NVENC + FFmpeg D3D11VA integration — Task 3 (`new_d3d11` with pool size 6, all settings)
- §6 Capture refactor — Task 5 (trait), Task 6 (DXGI), Task 7 (WGC), Task 1 (existing CPU path preserved as `LegacyVideoProcessor`)
- §7 Fallback + user notification — Task 9 (event), Task 10 (selection logic), Task 11 (React toast)
- §8 Verification + code layout — Task 12 (manual verification)
- §9 Out-of-scope — explicitly not implemented
- §10 Notes for the Implementation Plan — followed; encoder first (Tasks 3-4), capture second (5-7), pipeline third (8), wiring fourth (10), toast fifth (11), verification sixth (12)
