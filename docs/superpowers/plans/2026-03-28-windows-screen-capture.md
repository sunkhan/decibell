# Windows Screen Capture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement Windows screen/window capture using WGC API with GPU-side BGRA→NV12 conversion, outputting `RawFrame` NV12 data through the existing video pipeline channel.

**Architecture:** Windows Graphics Capture API acquires BGRA textures, D3D11 Video Processor converts them to NV12 on the GPU, a staging texture enables CPU readback of the NV12 data. Source enumeration uses DXGI for monitors and `EnumWindows` for windows. The capture module implements the same `list_sources()` / `start_capture()` interface as the Linux `capture_pipewire.rs`.

**Tech Stack:** `windows` crate (Microsoft official Rust bindings), D3D11, DXGI, Windows Graphics Capture API, WinRT interop.

**Spec:** `docs/superpowers/specs/2026-03-28-windows-screen-capture-design.md`

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `tauri-client/src-tauri/Cargo.toml` | Modify | Replace `windows-capture` with `windows` crate + features |
| `tauri-client/src-tauri/src/media/capture_wgc.rs` | Create | Windows capture module: source enumeration, WGC capture, D3D11 video processor, NV12 readback |

No other files need modification — `mod.rs` already declares `capture_wgc` behind `#[cfg(target_os = "windows")]`, and `capture.rs` already dispatches to it.

---

### Task 1: Update Cargo.toml Dependencies

**Files:**
- Modify: `tauri-client/src-tauri/Cargo.toml:39-40`

- [ ] **Step 1: Replace `windows-capture` with `windows` crate**

Replace the Windows dependencies section:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
windows = { version = "0.58", features = [
    "Graphics_Capture",
    "Graphics_DirectX",
    "Graphics_DirectX_Direct3D11",
    "Win32_Graphics_Direct3D",
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Dxgi",
    "Win32_System_WinRT",
    "Win32_System_WinRT_Direct3D11",
    "Win32_System_WinRT_Graphics_Capture",
    "Win32_UI_WindowsAndMessaging",
    "Win32_Foundation",
    "Foundation",
    "Win32_Networking_WinSock",
] }
```

- [ ] **Step 2: Verify it compiles on current platform**

Run: `cargo check 2>&1 | head -20`

This will succeed on Linux (the Windows deps are behind `cfg` and won't be resolved). The real compilation test happens on Windows.

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/Cargo.toml
git commit -m "build: replace windows-capture with windows crate for WGC capture"
```

---

### Task 2: Source Enumeration — Monitors via DXGI

**Files:**
- Create: `tauri-client/src-tauri/src/media/capture_wgc.rs`

- [ ] **Step 1: Create the module with monitor enumeration**

Create `capture_wgc.rs` with DXGI monitor enumeration and the `list_sources()` public function:

```rust
use std::sync::mpsc::SyncSender;
use std::time::Instant;

use super::capture::{CaptureConfig, CaptureSource, CaptureSourceType, RawFrame};

use windows::{
    core::*,
    Graphics::Capture::*,
    Graphics::DirectX::Direct3D11::IDirect3DDevice,
    Graphics::DirectX::DirectXPixelFormat,
    Graphics::SizeInt32,
    Win32::Foundation::*,
    Win32::Graphics::Direct3D::*,
    Win32::Graphics::Direct3D11::*,
    Win32::Graphics::Dxgi::*,
    Win32::System::WinRT::Direct3D11::*,
    Win32::System::WinRT::Graphics::Capture::*,
    Win32::UI::WindowsAndMessaging::*,
};

// ─── Source Enumeration ──────────────────────────────────────────────────────

/// List available capture sources (monitors and windows).
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    let mut sources = Vec::new();

    // Enumerate monitors via DXGI
    enumerate_monitors(&mut sources).map_err(|e| format!("Monitor enumeration: {}", e))?;

    // Enumerate windows via EnumWindows
    enumerate_windows(&mut sources);

    Ok(sources)
}

/// Enumerate monitors using DXGI Factory → Adapters → Outputs.
fn enumerate_monitors(sources: &mut Vec<CaptureSource>) -> Result<(), windows::core::Error> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
        let mut adapter_idx: u32 = 0;

        loop {
            let adapter = match factory.EnumAdapters1(adapter_idx) {
                Ok(a) => a,
                Err(_) => break, // No more adapters
            };

            let mut output_idx: u32 = 0;
            loop {
                let output = match adapter.EnumOutputs(output_idx) {
                    Ok(o) => o,
                    Err(_) => break, // No more outputs on this adapter
                };

                let desc = output.GetDesc()?;
                let name = String::from_utf16_lossy(
                    &desc.DeviceName[..desc.DeviceName.iter().position(|&c| c == 0).unwrap_or(desc.DeviceName.len())]
                );
                let rect = desc.DesktopCoordinates;
                let width = (rect.right - rect.left) as u32;
                let height = (rect.bottom - rect.top) as u32;

                sources.push(CaptureSource {
                    id: format!("monitor:{}:{}", adapter_idx, output_idx),
                    name: if name.is_empty() {
                        format!("Monitor {}", sources.len() + 1)
                    } else {
                        name
                    },
                    source_type: CaptureSourceType::Screen,
                    width,
                    height,
                });

                output_idx += 1;
            }

            adapter_idx += 1;
        }
    }

    Ok(())
}
```

- [ ] **Step 2: Commit**

```bash
git add tauri-client/src-tauri/src/media/capture_wgc.rs
git commit -m "feat(capture): add DXGI monitor enumeration for Windows"
```

---

### Task 3: Source Enumeration — Windows via EnumWindows

**Files:**
- Modify: `tauri-client/src-tauri/src/media/capture_wgc.rs`

- [ ] **Step 1: Add window enumeration function**

Add after `enumerate_monitors`:

```rust
/// Enumerate visible, capturable windows using EnumWindows.
fn enumerate_windows(sources: &mut Vec<CaptureSource>) {
    unsafe {
        let mut hwnds: Vec<HWND> = Vec::new();

        // EnumWindows calls our callback for each top-level window
        let _ = EnumWindows(
            Some(enum_windows_callback),
            LPARAM(&mut hwnds as *mut Vec<HWND> as isize),
        );

        for hwnd in hwnds {
            let mut title = [0u16; 512];
            let len = GetWindowTextW(hwnd, &mut title);
            if len == 0 {
                continue;
            }
            let name = String::from_utf16_lossy(&title[..len as usize]);

            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                continue;
            }
            let width = (rect.right - rect.left) as u32;
            let height = (rect.bottom - rect.top) as u32;

            // Skip tiny windows (likely invisible helpers)
            if width < 100 || height < 100 {
                continue;
            }

            sources.push(CaptureSource {
                id: format!("window:{}", hwnd.0 as usize),
                name,
                source_type: CaptureSourceType::Window,
                width,
                height,
            });
        }
    }
}

/// Callback for EnumWindows — filters and collects visible, capturable windows.
unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let hwnds = &mut *(lparam.0 as *mut Vec<HWND>);

    // Skip invisible windows
    if !IsWindowVisible(hwnd).as_bool() {
        return TRUE;
    }

    // Skip minimized windows
    if IsIconic(hwnd).as_bool() {
        return TRUE;
    }

    // Must have a title
    let len = GetWindowTextLengthW(hwnd);
    if len == 0 {
        return TRUE;
    }

    // Must have WS_CAPTION style (filters out tool windows, overlays, etc.)
    let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
    if style & WS_CAPTION.0 != WS_CAPTION.0 {
        return TRUE;
    }

    // Skip windows with WS_EX_TOOLWINDOW extended style
    let ex_style = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
    if ex_style & WS_EX_TOOLWINDOW.0 != 0 {
        return TRUE;
    }

    hwnds.push(hwnd);
    TRUE
}
```

- [ ] **Step 2: Commit**

```bash
git add tauri-client/src-tauri/src/media/capture_wgc.rs
git commit -m "feat(capture): add window enumeration via EnumWindows for Windows"
```

---

### Task 4: D3D11 Device Creation and WinRT Interop

**Files:**
- Modify: `tauri-client/src-tauri/src/media/capture_wgc.rs`

- [ ] **Step 1: Add D3D11 device creation and WinRT interop helpers**

Add after the enumeration functions:

```rust
// ─── D3D11 Setup ─────────────────────────────────────────────────────────────

/// Create a D3D11 device suitable for WGC capture and video processing.
fn create_d3d11_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT; // Required for WGC interop
    let feature_levels = [D3D_FEATURE_LEVEL_11_0];

    let mut device = None;
    let mut context = None;

    unsafe {
        D3D11CreateDevice(
            None,                          // Default adapter
            D3D_DRIVER_TYPE_HARDWARE,
            None,                          // No software rasterizer
            flags,
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,                          // Don't need actual feature level
            Some(&mut context),
        )
        .map_err(|e| format!("D3D11CreateDevice: {}", e))?;
    }

    Ok((
        device.ok_or("D3D11 device is None")?,
        context.ok_or("D3D11 context is None")?,
    ))
}

/// Convert a D3D11 device to a WinRT IDirect3DDevice for WGC interop.
fn create_winrt_device(device: &ID3D11Device) -> Result<IDirect3DDevice, String> {
    unsafe {
        let dxgi_device: IDXGIDevice = device.cast()
            .map_err(|e| format!("Cast to IDXGIDevice: {}", e))?;

        let inspectable = CreateDirect3D11DeviceFromDXGIDevice(&dxgi_device)
            .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice: {}", e))?;

        inspectable.cast()
            .map_err(|e| format!("Cast to IDirect3DDevice: {}", e))
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add tauri-client/src-tauri/src/media/capture_wgc.rs
git commit -m "feat(capture): add D3D11 device creation and WinRT interop for Windows"
```

---

### Task 5: GraphicsCaptureItem Creation from Source ID

**Files:**
- Modify: `tauri-client/src-tauri/src/media/capture_wgc.rs`

- [ ] **Step 1: Add capture item creation from monitor or window source ID**

Add after the D3D11 helpers:

```rust
// ─── Capture Item Creation ───────────────────────────────────────────────────

/// Create a GraphicsCaptureItem from a source ID string.
/// Supports "monitor:<adapter>:<output>" and "window:<hwnd>" formats.
fn create_capture_item(source_id: &str) -> Result<GraphicsCaptureItem, String> {
    if let Some(rest) = source_id.strip_prefix("monitor:") {
        create_capture_item_for_monitor(rest)
    } else if let Some(rest) = source_id.strip_prefix("window:") {
        create_capture_item_for_window(rest)
    } else {
        Err(format!("Unknown source ID format: {}", source_id))
    }
}

/// Create a GraphicsCaptureItem for a monitor identified by "adapter_idx:output_idx".
fn create_capture_item_for_monitor(id: &str) -> Result<GraphicsCaptureItem, String> {
    let parts: Vec<&str> = id.split(':').collect();
    if parts.len() != 2 {
        return Err(format!("Invalid monitor ID: {}", id));
    }
    let adapter_idx: u32 = parts[0].parse().map_err(|_| "Invalid adapter index")?;
    let output_idx: u32 = parts[1].parse().map_err(|_| "Invalid output index")?;

    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1()
            .map_err(|e| format!("CreateDXGIFactory1: {}", e))?;
        let adapter = factory.EnumAdapters1(adapter_idx)
            .map_err(|e| format!("EnumAdapters1({}): {}", adapter_idx, e))?;
        let output = adapter.EnumOutputs(output_idx)
            .map_err(|e| format!("EnumOutputs({}): {}", output_idx, e))?;

        // Get the HMONITOR from the output description
        let desc = output.GetDesc()
            .map_err(|e| format!("GetDesc: {}", e))?;
        let hmonitor = desc.Monitor;

        let interop: IGraphicsCaptureItemInterop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("CaptureItem interop factory: {}", e))?;

        interop.CreateForMonitor(hmonitor)
            .map_err(|e| format!("CreateForMonitor: {}", e))
    }
}

/// Create a GraphicsCaptureItem for a window identified by HWND decimal value.
fn create_capture_item_for_window(id: &str) -> Result<GraphicsCaptureItem, String> {
    let hwnd_val: usize = id.parse().map_err(|_| format!("Invalid HWND: {}", id))?;
    let hwnd = HWND(hwnd_val as *mut _);

    unsafe {
        if !IsWindow(hwnd).as_bool() {
            return Err(format!("HWND {} is not a valid window", id));
        }

        let interop: IGraphicsCaptureItemInterop = windows::core::factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| format!("CaptureItem interop factory: {}", e))?;

        interop.CreateForWindow(hwnd)
            .map_err(|e| format!("CreateForWindow: {}", e))
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add tauri-client/src-tauri/src/media/capture_wgc.rs
git commit -m "feat(capture): add GraphicsCaptureItem creation from source ID"
```

---

### Task 6: D3D11 Video Processor for BGRA→NV12 Conversion

**Files:**
- Modify: `tauri-client/src-tauri/src/media/capture_wgc.rs`

- [ ] **Step 1: Add video processor setup and per-frame conversion**

Add after the capture item creation functions:

```rust
// ─── D3D11 Video Processor (BGRA → NV12) ────────────────────────────────────

/// Holds the D3D11 Video Processor resources for BGRA→NV12 conversion.
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

impl VideoProcessor {
    /// Create a video processor for BGRA→NV12 conversion.
    /// `src_w/src_h` = capture source resolution, `dst_w/dst_h` = output resolution.
    fn new(
        device: &ID3D11Device,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<Self, String> {
        unsafe {
            let video_device: ID3D11VideoDevice = device.cast()
                .map_err(|e| format!("Cast to ID3D11VideoDevice: {}", e))?;

            let context = {
                let mut ctx = None;
                device.GetImmediateContext(&mut ctx);
                ctx.ok_or("No immediate context")?
            };
            let video_context: ID3D11VideoContext = context.cast()
                .map_err(|e| format!("Cast to ID3D11VideoContext: {}", e))?;

            // Create enumerator describing input/output formats
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

            let enumerator = video_device.CreateVideoProcessorEnumerator(&content_desc)
                .map_err(|e| format!("CreateVideoProcessorEnumerator: {}", e))?;

            let processor = video_device.CreateVideoProcessor(&enumerator, 0)
                .map_err(|e| format!("CreateVideoProcessor: {}", e))?;

            // Create NV12 output texture (GPU, render target for video processor output)
            let nv12_desc = D3D11_TEXTURE2D_DESC {
                Width: dst_w,
                Height: dst_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET,
                CPUAccessFlags: D3D11_CPU_ACCESS_FLAG(0),
                MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
            };
            let nv12_texture = device.CreateTexture2D(&nv12_desc, None)
                .map_err(|e| format!("Create NV12 texture: {}", e))?;

            // Create staging texture for CPU readback
            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: dst_w,
                Height: dst_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: D3D11_BIND_FLAG(0),
                CPUAccessFlags: D3D11_CPU_ACCESS_READ,
                MiscFlags: D3D11_RESOURCE_MISC_FLAG(0),
            };
            let staging_texture = device.CreateTexture2D(&staging_desc, None)
                .map_err(|e| format!("Create staging texture: {}", e))?;

            // Create output view on the NV12 texture
            let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let output_view = video_device.CreateVideoProcessorOutputView(
                &nv12_texture, &enumerator, &output_view_desc,
            ).map_err(|e| format!("Create output view: {}", e))?;

            Ok(Self {
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

    /// Convert a BGRA capture texture to NV12 and read back to CPU memory.
    /// Returns tightly-packed NV12 data: [Y plane (w*h bytes)] [UV plane (w*h/2 bytes)].
    fn convert_and_readback(
        &self,
        context: &ID3D11DeviceContext,
        bgra_texture: &ID3D11Texture2D,
    ) -> Result<Vec<u8>, String> {
        unsafe {
            // Create input view for this frame's BGRA texture
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
            let input_view = self.video_device.CreateVideoProcessorInputView(
                bgra_texture, &self.enumerator, &input_view_desc,
            ).map_err(|e| format!("Create input view: {}", e))?;

            // Build the stream descriptor
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: TRUE,
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

            // Run the video processor (BGRA → NV12 + optional scaling)
            self.video_context.VideoProcessorBlt(
                &self.processor,
                &self.output_view,
                0,
                &[stream],
            ).map_err(|e| format!("VideoProcessorBlt: {}", e))?;

            // Copy NV12 result to staging texture for CPU read
            context.CopyResource(&self.staging_texture, &self.nv12_texture);

            // Map staging texture and read NV12 data
            let mapped = context.Map(&self.staging_texture, 0, D3D11_MAP_READ, 0)
                .map_err(|e| format!("Map staging: {}", e))?;

            let w = self.output_width as usize;
            let h = self.output_height as usize;
            let y_size = w * h;
            let uv_size = w * (h / 2);
            let mut nv12 = Vec::with_capacity(y_size + uv_size);

            let row_pitch = mapped.RowPitch as usize;
            let src = mapped.pData as *const u8;

            // Copy Y plane (first h rows)
            if row_pitch == w {
                // Tightly packed — single memcpy
                nv12.extend_from_slice(std::slice::from_raw_parts(src, y_size));
            } else {
                // Row pitch differs — copy row by row
                for row in 0..h {
                    let row_start = src.add(row * row_pitch);
                    nv12.extend_from_slice(std::slice::from_raw_parts(row_start, w));
                }
            }

            // Copy UV plane (next h/2 rows, starting at offset h * row_pitch in NV12 layout)
            // In D3D11 NV12 textures, UV plane starts at row_pitch * height
            let uv_base = src.add(h * row_pitch);
            if row_pitch == w {
                nv12.extend_from_slice(std::slice::from_raw_parts(uv_base, uv_size));
            } else {
                for row in 0..(h / 2) {
                    let row_start = uv_base.add(row * row_pitch);
                    nv12.extend_from_slice(std::slice::from_raw_parts(row_start, w));
                }
            }

            context.Unmap(&self.staging_texture, 0);

            Ok(nv12)
        }
    }
}
```

- [ ] **Step 2: Commit**

```bash
git add tauri-client/src-tauri/src/media/capture_wgc.rs
git commit -m "feat(capture): add D3D11 video processor for GPU BGRA→NV12 conversion"
```

---

### Task 7: WGC Capture Session and Frame Loop

**Files:**
- Modify: `tauri-client/src-tauri/src/media/capture_wgc.rs`

- [ ] **Step 1: Add the `start_capture` function and capture loop**

Add the `start_capture` public function and the capture thread logic:

```rust
// ─── Capture Session ─────────────────────────────────────────────────────────

/// Start capturing from the given source ID.
/// Returns a channel receiver that yields NV12 RawFrames.
pub async fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<std::sync::mpsc::Receiver<RawFrame>, String> {
    let source_id = source_id.to_string();
    let config = config.clone();

    let rx = tokio::task::spawn_blocking(move || -> Result<_, String> {
        let (tx, rx) = std::sync::mpsc::sync_channel::<RawFrame>(4);

        // Create D3D11 device
        let (device, context) = create_d3d11_device()?;
        let winrt_device = create_winrt_device(&device)?;

        // Create capture item from source ID
        let item = create_capture_item(&source_id)?;
        let item_size = item.Size().map_err(|e| format!("Item size: {}", e))?;
        let src_w = item_size.Width as u32;
        let src_h = item_size.Height as u32;

        eprintln!(
            "[capture] WGC source: {}x{}, target: {}x{} @ {}fps",
            src_w, src_h, config.target_width, config.target_height, config.target_fps
        );

        // Determine output resolution
        let dst_w = if config.target_width == 0 { src_w } else { config.target_width };
        let dst_h = if config.target_height == 0 { src_h } else { config.target_height };

        // Create video processor for BGRA→NV12
        let video_proc = VideoProcessor::new(&device, src_w, src_h, dst_w, dst_h)?;

        // Spawn capture thread
        std::thread::Builder::new()
            .name("decibell-capture".to_string())
            .spawn(move || {
                if let Err(e) = wgc_capture_loop(
                    device, context, winrt_device, item,
                    video_proc, tx, config, dst_w, dst_h,
                ) {
                    eprintln!("[capture] WGC capture error: {}", e);
                }
            })
            .map_err(|e| format!("Spawn capture thread: {}", e))?;

        Ok(rx)
    })
    .await
    .map_err(|e| format!("Join error: {}", e))??;

    Ok(rx)
}

/// Run the WGC capture loop on a dedicated thread.
fn wgc_capture_loop(
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    winrt_device: IDirect3DDevice,
    item: GraphicsCaptureItem,
    video_proc: VideoProcessor,
    tx: SyncSender<RawFrame>,
    config: CaptureConfig,
    dst_w: u32,
    dst_h: u32,
) -> Result<(), String> {
    let item_size = item.Size().map_err(|e| format!("Item size: {}", e))?;

    // Create frame pool (2 frames, BGRA format)
    let frame_pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        item_size,
    ).map_err(|e| format!("Create frame pool: {}", e))?;

    // Create capture session
    let session = frame_pool.CreateCaptureSession(&item)
        .map_err(|e| format!("Create capture session: {}", e))?;

    // Try to disable the yellow capture border (Windows 11+)
    if let Ok(session2) = session.cast::<IGraphicsCaptureSession2>() {
        let _ = session2.SetIsCursorCaptureEnabled(true);
    }
    if let Ok(session3) = session.cast::<IGraphicsCaptureSession3>() {
        let _ = session3.SetIsBorderRequired(false);
    }

    // Start the session
    session.StartCapture().map_err(|e| format!("StartCapture: {}", e))?;
    eprintln!("[capture] WGC session started");

    let start = Instant::now();
    let mut frame_count: u64 = 0;

    // Poll loop: TryGetNextFrame with a short sleep to avoid busy-waiting
    loop {
        let frame = match frame_pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(_) => {
                // No frame available yet
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
        };

        frame_count += 1;

        // Extract the D3D11 texture from the WinRT surface
        let surface = frame.Surface().map_err(|e| format!("Frame surface: {}", e))?;
        let access: IDirect3DDxgiInterfaceAccess = surface.cast()
            .map_err(|e| format!("Cast to DxgiInterfaceAccess: {}", e))?;
        let bgra_texture: ID3D11Texture2D = unsafe {
            access.GetInterface()
                .map_err(|e| format!("GetInterface<ID3D11Texture2D>: {}", e))?
        };

        if frame_count <= 3 || frame_count % 120 == 0 {
            eprintln!(
                "[capture] Frame {} ({}x{} -> {}x{}, {:.1}s)",
                frame_count, item_size.Width, item_size.Height,
                dst_w, dst_h, start.elapsed().as_secs_f64()
            );
        }

        // GPU BGRA→NV12 conversion + CPU readback
        let nv12 = match video_proc.convert_and_readback(&context, &bgra_texture) {
            Ok(data) => data,
            Err(e) => {
                if frame_count <= 3 {
                    eprintln!("[capture] Convert error: {}", e);
                }
                continue;
            }
        };

        // Drop the frame (releases the texture back to the frame pool)
        drop(frame);

        let raw_frame = RawFrame {
            data: nv12,
            width: dst_w,
            height: dst_h,
            timestamp_us: start.elapsed().as_micros() as u64,
        };

        match tx.try_send(raw_frame) {
            Ok(()) => {}
            Err(std::sync::mpsc::TrySendError::Full(_)) => {} // Drop frame
            Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                eprintln!("[capture] Channel closed, stopping capture");
                break;
            }
        }
    }

    // Cleanup: stop session and close frame pool
    session.Close().ok();
    frame_pool.Close().ok();
    eprintln!("[capture] WGC capture stopped");

    Ok(())
}
```

- [ ] **Step 2: Commit**

```bash
git add tauri-client/src-tauri/src/media/capture_wgc.rs
git commit -m "feat(capture): add WGC capture session and frame loop with NV12 output"
```

---

### Task 8: Windows Socket Buffer Size

**Files:**
- Modify: `tauri-client/src-tauri/src/media/mod.rs:63-76`

The existing socket buffer increase only runs on `#[cfg(unix)]`. Windows needs the same for video keyframe bursts.

- [ ] **Step 1: Add Windows socket buffer size increase**

After the existing `#[cfg(unix)]` block (line 76), add:

```rust
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            let sock = socket.as_raw_socket();
            let buf_size: i32 = 4 * 1024 * 1024; // 4 MB
            unsafe {
                let _ = windows::Win32::Networking::WinSock::setsockopt(
                    windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                    windows::Win32::Networking::WinSock::SOL_SOCKET as i32,
                    windows::Win32::Networking::WinSock::SO_RCVBUF as i32,
                    Some(std::slice::from_raw_parts(
                        &buf_size as *const i32 as *const u8,
                        std::mem::size_of::<i32>(),
                    )),
                );
                let _ = windows::Win32::Networking::WinSock::setsockopt(
                    windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                    windows::Win32::Networking::WinSock::SOL_SOCKET as i32,
                    windows::Win32::Networking::WinSock::SO_SNDBUF as i32,
                    Some(std::slice::from_raw_parts(
                        &buf_size as *const i32 as *const u8,
                        std::mem::size_of::<i32>(),
                    )),
                );
            }
        }
```

**Note:** `Win32_Networking_WinSock` is already included in the `windows` crate features from Task 1.

- [ ] **Step 2: Commit**

```bash
git add tauri-client/src-tauri/src/media/mod.rs tauri-client/src-tauri/Cargo.toml
git commit -m "feat: add Windows UDP socket buffer size increase for video streaming"
```

---

### Task 9: Verify Full Compilation on Linux

**Files:** None (verification only)

- [ ] **Step 1: Run `cargo check` to verify the project compiles on Linux**

Run: `cargo check 2>&1 | head -30`

Expected: No new errors. The `capture_wgc.rs` module is behind `#[cfg(target_os = "windows")]` so it won't be compiled on Linux. The `mod.rs` Windows socket code is behind `#[cfg(windows)]`. Only the `Cargo.toml` changes affect Linux, but Windows-only deps are conditional.

- [ ] **Step 2: Verify no unintended changes to Linux capture**

Run: `cargo check 2>&1 | grep -i error`

Expected: No errors.

---

### Task 10: Final Review and Integration Commit

**Files:** All files from previous tasks

- [ ] **Step 1: Review the complete `capture_wgc.rs` for consistency**

Verify:
- `list_sources()` returns `Result<Vec<CaptureSource>, String>` matching `capture.rs`
- `start_capture()` returns `Result<std::sync::mpsc::Receiver<RawFrame>, String>` matching `capture.rs`
- NV12 output format matches what `encoder.rs` expects (tightly-packed Y + UV planes)
- Channel uses `sync_channel(4)` matching Linux's buffer size
- Shutdown on `TrySendError::Disconnected` matches Linux behavior
- `GraphicsCaptureItem::Closed` event is handled (source disappearing)

- [ ] **Step 2: Add Closed event handling if missing**

In `wgc_capture_loop`, before the poll loop, add a `Closed` handler on the `item`:

```rust
    // Handle source disappearing (monitor disconnected, window closed)
    let closed_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let closed_flag_clone = closed_flag.clone();
    let _closed_token = item.Closed(&windows::Foundation::TypedEventHandler::new(
        move |_, _| {
            eprintln!("[capture] Capture source closed");
            closed_flag_clone.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        },
    )).map_err(|e| format!("Register Closed handler: {}", e))?;
```

Then in the poll loop, check the flag after each frame attempt:

```rust
        // Check if the source was closed
        if closed_flag.load(std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[capture] Source closed, stopping capture");
            break;
        }
```

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src-tauri/src/media/capture_wgc.rs
git commit -m "feat(capture): add source-closed handling for WGC capture"
```
