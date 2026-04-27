# DXGI Desktop Duplication Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace WGC with DXGI Desktop Duplication for Windows screen capture, delivering continuous NV12 frames at display refresh rate with minimal CPU usage.

**Architecture:** A new `capture_dxgi.rs` module implements the `list_sources`/`start_capture` interface using DXGI OutputDuplication API. The capture loop runs on a dedicated thread, acquiring frames from the desktop compositor via `AcquireNextFrame`, converting BGRA→NV12 on the GPU using D3D11 Video Processor, and reading back NV12 data from a staging texture. Window capture still uses WGC since DXGI DD only captures full monitors. The routing logic in `capture.rs` dispatches to the correct backend based on source ID prefix (`monitor:` → DXGI DD, `window:` → WGC).

**Tech Stack:** `windows` crate 0.58, DXGI 1.2 OutputDuplication API, D3D11 Video Processor, existing `capture.rs` types (`RawFrame`, `CaptureSource`, `CaptureConfig`).

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `src/media/capture_dxgi.rs` | **Create** | DXGI Desktop Duplication: source enumeration + capture loop |
| `src/media/capture.rs` | **Modify** | Route `monitor:` sources to DXGI DD, `window:` to WGC |
| `src/media/capture_wgc.rs` | **Modify** | Remove monitor enumeration/capture (keep window-only) |
| `src/media/mod.rs` | **Modify** | Add `capture_dxgi` module declaration |
| `Cargo.toml` | **Modify** | Add `Win32_Security` feature for DXGI DD |

---

### Task 1: Add `capture_dxgi` module declaration and Cargo features

**Files:**
- Modify: `src-tauri/src/media/mod.rs`
- Modify: `src-tauri/Cargo.toml`

- [ ] **Step 1: Add module declaration in mod.rs**

In `src/media/mod.rs`, add after the `capture_wgc` line:

```rust
#[cfg(target_os = "windows")]
pub mod capture_dxgi;
```

- [ ] **Step 2: Add Win32_Security and Win32_System_Threading features to Cargo.toml**

DXGI Desktop Duplication requires `IDXGIOutput1` (already in `Win32_Graphics_Dxgi`) and `SECURITY_ATTRIBUTES` from `Win32_Security`. Add to the windows features list in Cargo.toml:

```toml
    "Win32_Security",
    "Win32_System_Threading",
```

- [ ] **Step 3: Create empty capture_dxgi.rs placeholder**

Create `src/media/capture_dxgi.rs` with just the public function signatures:

```rust
use super::capture::{CaptureConfig, CaptureSource, CaptureSourceType, RawFrame};

/// List available monitors via DXGI enumeration.
pub fn list_sources() -> Result<Vec<CaptureSource>, String> {
    Ok(Vec::new()) // placeholder
}

/// Start DXGI Desktop Duplication capture on a monitor.
pub fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<std::sync::mpsc::Receiver<RawFrame>, String> {
    let _ = (source_id, config);
    Err("Not yet implemented".to_string())
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check` (with VCPKG_ROOT and CMAKE_POLICY_VERSION_MINIMUM set)
Expected: compiles with warnings only

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/media/mod.rs src-tauri/src/media/capture_dxgi.rs src-tauri/Cargo.toml
git commit -m "feat(capture): scaffold capture_dxgi module for DXGI Desktop Duplication"
```

---

### Task 2: Implement monitor enumeration in capture_dxgi.rs

**Files:**
- Modify: `src-tauri/src/media/capture_dxgi.rs`

- [ ] **Step 1: Implement list_sources**

Replace the placeholder `list_sources` with DXGI adapter/output enumeration. This is the same DXGI enumeration logic from `capture_wgc.rs` but lives in its own module:

```rust
use super::capture::{CaptureConfig, CaptureSource, CaptureSourceType, RawFrame};

use windows::{
    core::Interface,
    Win32::Graphics::Direct3D::*,
    Win32::Graphics::Direct3D11::*,
    Win32::Graphics::Dxgi::*,
    Win32::Graphics::Dxgi::Common::*,
    Win32::Foundation::*,
};

/// List available monitors via DXGI enumeration.
pub fn list_sources() -> Result<Vec<CaptureSource>, String> {
    let mut sources = Vec::new();

    unsafe {
        let factory: IDXGIFactory1 =
            CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {}", e))?;

        let mut adapter_idx: u32 = 0;
        loop {
            let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(adapter_idx) {
                Ok(a) => a,
                Err(_) => break,
            };

            let mut output_idx: u32 = 0;
            loop {
                let output: IDXGIOutput = match adapter.EnumOutputs(output_idx) {
                    Ok(o) => o,
                    Err(_) => break,
                };

                let desc = match output.GetDesc() {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("[capture-dxgi] GetDesc failed: {}", e);
                        output_idx += 1;
                        continue;
                    }
                };

                let coords = desc.DesktopCoordinates;
                let width = (coords.right - coords.left).unsigned_abs();
                let height = (coords.bottom - coords.top).unsigned_abs();

                let name_raw = &desc.DeviceName;
                let end = name_raw.iter().position(|&c| c == 0).unwrap_or(name_raw.len());
                let name = if end > 0 {
                    String::from_utf16_lossy(&name_raw[..end])
                } else {
                    format!("Monitor {}", sources.len() + 1)
                };

                sources.push(CaptureSource {
                    id: format!("monitor:{}:{}", adapter_idx, output_idx),
                    name,
                    source_type: CaptureSourceType::Screen,
                    width,
                    height,
                });

                output_idx += 1;
            }

            adapter_idx += 1;
        }
    }

    Ok(sources)
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/media/capture_dxgi.rs
git commit -m "feat(capture): implement DXGI monitor enumeration"
```

---

### Task 3: Implement the DXGI Desktop Duplication capture loop

This is the core task. The capture loop:
1. Creates a D3D11 device on the same adapter as the target output
2. Gets `IDXGIOutputDuplication` from `IDXGIOutput1::DuplicateOutput`
3. Calls `AcquireNextFrame` in a tight loop (with timeout = frame interval)
4. Converts acquired BGRA texture → NV12 via D3D11 Video Processor (GPU-side)
5. Reads back NV12 from staging texture to CPU
6. Sends `RawFrame` through the channel

**Files:**
- Modify: `src-tauri/src/media/capture_dxgi.rs`

- [ ] **Step 1: Add D3D11 device creation helper**

Add a function that creates a D3D11 device on a specific adapter (important: must be the same adapter that owns the output, or DuplicateOutput fails):

```rust
fn create_device_for_adapter(
    adapter: &IDXGIAdapter1,
) -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let feature_levels = [D3D_FEATURE_LEVEL_11_0];
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    let mut actual_level = D3D_FEATURE_LEVEL_11_0;

    unsafe {
        D3D11CreateDevice(
            adapter,
            D3D_DRIVER_TYPE_UNKNOWN, // must be UNKNOWN when specifying adapter
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            Some(&mut actual_level),
            Some(&mut context),
        )
        .map_err(|e| format!("D3D11CreateDevice: {}", e))?;
    }

    Ok((
        device.ok_or("D3D11CreateDevice returned None")?,
        context.ok_or("D3D11CreateDevice context None")?,
    ))
}
```

- [ ] **Step 2: Add VideoProcessor (reuse pattern from capture_wgc.rs)**

Add the same `VideoProcessor` struct for GPU BGRA→NV12 conversion. This is copied from `capture_wgc.rs` since it's the same D3D11 Video Processor logic:

```rust
struct VideoProcessor {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
    output_view: ID3D11VideoProcessorOutputView,
    nv12_texture: ID3D11Texture2D,
    staging_texture: ID3D11Texture2D,
    output_width: u32,
    output_height: u32,
}

unsafe impl Send for VideoProcessor {}

impl VideoProcessor {
    fn new(
        device: &ID3D11Device,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<Self, String> {
        unsafe {
            let video_device: ID3D11VideoDevice = device
                .cast()
                .map_err(|e| format!("Cast to ID3D11VideoDevice: {}", e))?;

            let base_context = device
                .GetImmediateContext()
                .map_err(|e| format!("GetImmediateContext: {}", e))?;

            let video_context: ID3D11VideoContext = base_context
                .cast()
                .map_err(|e| format!("Cast to ID3D11VideoContext: {}", e))?;

            let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: src_w,
                InputHeight: src_h,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: dst_w,
                OutputHeight: dst_h,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };

            let enumerator = video_device
                .CreateVideoProcessorEnumerator(&content_desc)
                .map_err(|e| format!("CreateVideoProcessorEnumerator: {}", e))?;

            let processor = video_device
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(|e| format!("CreateVideoProcessor: {}", e))?;

            let nv12_desc = D3D11_TEXTURE2D_DESC {
                Width: dst_w,
                Height: dst_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };

            let mut nv12_texture: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&nv12_desc, None, Some(&mut nv12_texture))
                .map_err(|e| format!("CreateTexture2D (NV12): {}", e))?;
            let nv12_texture = nv12_texture.ok_or("CreateTexture2D (NV12) returned None")?;

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: dst_w,
                Height: dst_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };

            let mut staging_texture: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging_texture))
                .map_err(|e| format!("CreateTexture2D (staging): {}", e))?;
            let staging_texture = staging_texture.ok_or("CreateTexture2D (staging) returned None")?;

            let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };

            let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
            video_device
                .CreateVideoProcessorOutputView(
                    &nv12_texture,
                    &enumerator,
                    &output_view_desc,
                    Some(&mut output_view),
                )
                .map_err(|e| format!("CreateVideoProcessorOutputView: {}", e))?;
            let output_view = output_view.ok_or("CreateVideoProcessorOutputView returned None")?;

            Ok(VideoProcessor {
                video_device,
                video_context,
                processor,
                enumerator,
                output_view,
                nv12_texture,
                staging_texture,
                output_width: dst_w,
                output_height: dst_h,
            })
        }
    }

    fn convert_and_readback(
        &self,
        context: &ID3D11DeviceContext,
        bgra_texture: &ID3D11Texture2D,
    ) -> Result<Vec<u8>, String> {
        unsafe {
            let input_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
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
                    &self.output_view,
                    0,
                    &[stream],
                )
                .map_err(|e| format!("VideoProcessorBlt: {}", e))?;

            context.CopyResource(&self.staging_texture, &self.nv12_texture);

            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            context
                .Map(
                    &self.staging_texture,
                    0,
                    D3D11_MAP_READ,
                    0,
                    Some(&mut mapped),
                )
                .map_err(|e| format!("Map staging: {}", e))?;

            let w = self.output_width as usize;
            let h = self.output_height as usize;
            let row_pitch = mapped.RowPitch as usize;
            let data_ptr = mapped.pData as *const u8;

            let y_size = w * h;
            let uv_size = w * h / 2;
            let mut nv12 = Vec::with_capacity(y_size + uv_size);

            for row in 0..h {
                let src_row = std::slice::from_raw_parts(data_ptr.add(row * row_pitch), w);
                nv12.extend_from_slice(src_row);
            }

            let uv_base = data_ptr.add(h * row_pitch);
            for row in 0..(h / 2) {
                let src_row = std::slice::from_raw_parts(uv_base.add(row * row_pitch), w);
                nv12.extend_from_slice(src_row);
            }

            context.Unmap(&self.staging_texture, 0);

            Ok(nv12)
        }
    }
}
```

- [ ] **Step 3: Implement start_capture with DXGI Desktop Duplication**

Replace the placeholder `start_capture`:

```rust
/// Start DXGI Desktop Duplication capture on a monitor.
/// source_id format: "monitor:{adapter_idx}:{output_idx}"
pub fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<std::sync::mpsc::Receiver<RawFrame>, String> {
    let rest = source_id
        .strip_prefix("monitor:")
        .ok_or_else(|| format!("DXGI DD only supports monitor sources, got: {}", source_id))?;

    let mut parts = rest.splitn(2, ':');
    let adapter_idx: u32 = parts.next().and_then(|s| s.parse().ok()).ok_or("Invalid adapter_idx")?;
    let output_idx: u32 = parts.next().and_then(|s| s.parse().ok()).ok_or("Invalid output_idx")?;

    let target_fps = config.target_fps;
    let target_w = config.target_width;
    let target_h = config.target_height;

    let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(2);

    std::thread::Builder::new()
        .name("decibell-dxgi-capture".to_string())
        .spawn(move || {
            if let Err(e) = dxgi_capture_thread(adapter_idx, output_idx, target_fps, target_w, target_h, tx) {
                eprintln!("[capture-dxgi] Fatal error: {}", e);
            }
        })
        .map_err(|e| format!("Spawn DXGI capture thread: {}", e))?;

    Ok(rx)
}
```

- [ ] **Step 4: Implement the capture thread function**

This is the core DXGI DD loop:

```rust
fn dxgi_capture_thread(
    adapter_idx: u32,
    output_idx: u32,
    target_fps: u32,
    target_w: u32,
    target_h: u32,
    tx: std::sync::mpsc::SyncSender<RawFrame>,
) -> Result<(), String> {
    unsafe {
        // Get the specific adapter and output
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
            .map_err(|e| format!("Cast to IDXGIOutput1: {}", e))?;

        // Get output dimensions
        let desc = output.GetDesc().map_err(|e| format!("GetDesc: {}", e))?;
        let coords = desc.DesktopCoordinates;
        let src_w = (coords.right - coords.left).unsigned_abs();
        let src_h = (coords.bottom - coords.top).unsigned_abs();

        let dst_w = if target_w == 0 { src_w } else { target_w };
        let dst_h = if target_h == 0 { src_h } else { target_h };

        // Create D3D11 device on the SAME adapter as the output
        let (device, context) = create_device_for_adapter(&adapter)?;

        // Create output duplication
        let duplication: IDXGIOutputDuplication = output1
            .DuplicateOutput(&device)
            .map_err(|e| format!("DuplicateOutput: {}", e))?;

        // Create video processor for BGRA → NV12 + optional scale
        let video_proc = VideoProcessor::new(&device, src_w, src_h, dst_w, dst_h)?;

        eprintln!(
            "[capture-dxgi] Started: monitor {}:{}, {}x{} → {}x{} @ {}fps",
            adapter_idx, output_idx, src_w, src_h, dst_w, dst_h, target_fps
        );

        let frame_interval = std::time::Duration::from_micros(1_000_000 / target_fps as u64);
        let start = std::time::Instant::now();
        let mut frame_count: u64 = 0;
        // Timeout in ms for AcquireNextFrame. Use the frame interval so we
        // don't block longer than one frame period. Minimum 1ms.
        let acquire_timeout_ms = (frame_interval.as_millis() as u32).max(1);

        loop {
            // AcquireNextFrame — blocks until a new frame or timeout
            let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
            let mut desktop_resource: Option<IDXGIResource> = None;

            let hr = duplication.AcquireNextFrame(
                acquire_timeout_ms,
                &mut frame_info,
                &mut desktop_resource,
            );

            match hr {
                Ok(()) => {}
                Err(e) => {
                    let code = e.code().0 as u32;
                    // DXGI_ERROR_WAIT_TIMEOUT (0x887A0027) — no new frame yet
                    if code == 0x887A0027 {
                        continue;
                    }
                    // DXGI_ERROR_ACCESS_LOST (0x887A0026) — need to recreate
                    if code == 0x887A0026 {
                        eprintln!("[capture-dxgi] Access lost, stopping");
                        break;
                    }
                    eprintln!("[capture-dxgi] AcquireNextFrame error: {}", e);
                    break;
                }
            }

            let resource = match desktop_resource {
                Some(r) => r,
                None => {
                    let _ = duplication.ReleaseFrame();
                    continue;
                }
            };

            // Get the BGRA desktop texture from the resource
            let bgra_texture: ID3D11Texture2D = resource
                .cast()
                .map_err(|e| format!("Cast to ID3D11Texture2D: {}", e))?;

            // GPU convert BGRA → NV12
            let nv12 = match video_proc.convert_and_readback(&context, &bgra_texture) {
                Ok(data) => data,
                Err(e) => {
                    eprintln!("[capture-dxgi] convert error: {}", e);
                    let _ = duplication.ReleaseFrame();
                    continue;
                }
            };

            // MUST release the frame before acquiring the next one
            let _ = duplication.ReleaseFrame();

            frame_count += 1;
            let timestamp_us = start.elapsed().as_micros() as u64;

            if frame_count <= 3 || frame_count % 300 == 0 {
                eprintln!(
                    "[capture-dxgi] Frame {} ({:.1}s)",
                    frame_count,
                    start.elapsed().as_secs_f64()
                );
            }

            let raw_frame = RawFrame {
                data: nv12,
                width: dst_w,
                height: dst_h,
                timestamp_us,
            };

            match tx.try_send(raw_frame) {
                Ok(()) => {}
                Err(std::sync::mpsc::TrySendError::Full(_)) => {
                    // Encoder is behind — drop this frame (backpressure)
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    eprintln!("[capture-dxgi] Channel closed, stopping");
                    break;
                }
            }
        }

        eprintln!("[capture-dxgi] Capture loop exited after {} frames", frame_count);
        Ok(())
    }
}
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check`
Expected: compiles (the new module isn't wired in yet, but the types should all resolve)

- [ ] **Step 6: Commit**

```bash
git add src-tauri/src/media/capture_dxgi.rs
git commit -m "feat(capture): implement DXGI Desktop Duplication capture loop with GPU NV12 conversion"
```

---

### Task 4: Wire up the routing in capture.rs

Route monitor sources to DXGI DD, window sources to WGC.

**Files:**
- Modify: `src-tauri/src/media/capture.rs`

- [ ] **Step 1: Update list_sources to combine DXGI monitors + WGC windows**

Replace the Windows `list_sources` block:

```rust
/// List available capture sources (screens and windows).
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    #[cfg(target_os = "linux")]
    {
        super::capture_pipewire::list_sources().await
    }
    #[cfg(target_os = "windows")]
    {
        tokio::task::spawn_blocking(|| {
            // Monitors from DXGI DD, windows from WGC
            let mut sources = super::capture_dxgi::list_sources().unwrap_or_default();
            let mut windows = super::capture_wgc::list_window_sources().unwrap_or_default();
            sources.append(&mut windows);
            Ok(sources)
        })
        .await
        .map_err(|e| format!("Join error: {}", e))?
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err("Screen capture not supported on this platform".to_string())
    }
}
```

- [ ] **Step 2: Update start_capture to route based on source ID prefix**

Replace the Windows `start_capture` block:

```rust
/// Start capturing from a source.
/// Returns a channel that receives RawFrames.
pub async fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<std::sync::mpsc::Receiver<RawFrame>, String> {
    #[cfg(target_os = "linux")]
    {
        super::capture_pipewire::start_capture(source_id, config).await
    }
    #[cfg(target_os = "windows")]
    {
        if source_id.starts_with("monitor:") {
            // DXGI Desktop Duplication for monitors — continuous frames
            let source_id = source_id.to_string();
            let config = config.clone();
            tokio::task::spawn_blocking(move || {
                super::capture_dxgi::start_capture(&source_id, &config)
            })
            .await
            .map_err(|e| format!("Join error: {}", e))?
        } else {
            // WGC for window capture
            super::capture_wgc::start_capture(source_id, config).await
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (source_id, config);
        Err("Screen capture not supported on this platform".to_string())
    }
}
```

- [ ] **Step 3: Verify it compiles**

This will fail because `capture_wgc::list_window_sources` doesn't exist yet. That's expected — we add it in Task 5.

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/media/capture.rs
git commit -m "feat(capture): route monitor sources to DXGI DD, windows to WGC"
```

---

### Task 5: Refactor capture_wgc.rs to window-only

Strip monitor enumeration from WGC (it's now handled by DXGI DD) and expose a `list_window_sources` function.

**Files:**
- Modify: `src-tauri/src/media/capture_wgc.rs`

- [ ] **Step 1: Rename list_sources to list_window_sources and remove monitor enumeration**

Replace the `list_sources` function:

```rust
/// List available window capture sources (WGC only — monitors handled by DXGI DD).
pub async fn list_window_sources() -> Result<Vec<CaptureSource>, String> {
    tokio::task::spawn_blocking(|| {
        let mut sources = Vec::new();
        enumerate_windows(&mut sources);
        Ok(sources)
    })
    .await
    .map_err(|e| format!("Join error: {}", e))?
}
```

Remove the `enumerate_monitors` function entirely from `capture_wgc.rs` (it's no longer called).

Also remove `create_capture_item_for_monitor` since monitor capture now goes through DXGI DD. Update `create_capture_item` to only handle `window:` prefix:

```rust
fn create_capture_item(source_id: &str) -> Result<GraphicsCaptureItem, String> {
    if let Some(rest) = source_id.strip_prefix("window:") {
        create_capture_item_for_window(rest)
    } else {
        Err(format!("WGC capture only supports window: sources, got: {}", source_id))
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check`
Expected: compiles with warnings only

- [ ] **Step 3: Commit**

```bash
git add src-tauri/src/media/capture_wgc.rs
git commit -m "refactor(capture): strip monitor code from WGC, now window-only"
```

---

### Task 6: Build and smoke test

- [ ] **Step 1: Full build**

```bash
export VCPKG_ROOT=/c/dev/vcpkg
export CMAKE_POLICY_VERSION_MINIMUM=3.5
cargo build
```

Expected: builds successfully

- [ ] **Step 2: Run the app and test monitor capture**

```bash
cd tauri-client
npm run tauri dev
```

1. Start a stream selecting a monitor source
2. Verify the console logs show `[capture-dxgi] Started:` and continuous frame delivery
3. Verify frames are encoded and sent (look for `[encoder]` and `[video-pipeline]` logs)

- [ ] **Step 3: Test window capture still works**

1. Start a stream selecting a window source
2. Verify WGC capture logs appear: `[capture-wgc]`

- [ ] **Step 4: Commit any fixes if needed**

```bash
git add -u
git commit -m "fix(capture): DXGI DD integration fixes"
```
