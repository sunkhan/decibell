# Windows Screen Capture Pipeline

## Summary

Implement screen/window capture on Windows for Decibell's streaming pipeline using the Windows Graphics Capture (WGC) API with GPU-side BGRA-to-NV12 conversion via D3D11 Video Processor. The CPU never touches pixel data beyond a final NV12 memcpy, minimizing impact on gaming performance.

## Scope

- Windows capture module (`capture_wgc.rs`) implementing `list_sources()` and `start_capture()`
- Source enumeration for monitors (DXGI) and windows (EnumWindows)
- GPU color conversion and optional scaling via D3D11 Video Processor
- NV12 output through the existing `SyncSender<RawFrame>` channel

**Out of scope:** Multi-vendor encoder support (NVENC/AMF/QuickSync) is a separate effort that applies to both platforms. The existing NVENC encoder via FFmpeg is used as-is.

## Dependencies

Replace `windows-capture = "1"` in `Cargo.toml` with the `windows` crate (Microsoft's official Rust bindings) with these features:

- `Graphics_Capture` - WGC API
- `Graphics_DirectX_Direct3D11` - WinRT D3D11 interop
- `Win32_Graphics_Direct3D11` - D3D11 device, video processor, staging textures
- `Win32_Graphics_Dxgi` - Monitor/adapter enumeration
- `Win32_Graphics_Direct3D` - Shared D3D types
- `Win32_UI_WindowsAndMessaging` - EnumWindows, GetWindowText
- `Win32_Foundation` - HWND, HMONITOR, BOOL, RECT
- `Win32_System_WinRT_Direct3D11` - `CreateDirect3D11DeviceFromDXGIDevice` interop

## Architecture

### Data Flow

```
WGC FramePool (BGRA D3D11 Texture)
    |
    v
D3D11 Video Processor (BGRA -> NV12, optional scaling)
    |
    v
NV12 D3D11 Texture
    |
    v
Staging Texture (GPU -> CPU readable)
    |
    v
Map + memcpy -> RawFrame { data: Vec<u8>, width, height }
    |
    v
SyncSender<RawFrame> -> video pipeline (encode + packetize + UDP send)
```

The CPU only performs the final NV12 readback. At 1920x1080, NV12 is ~3.1MB/frame (vs ~8.3MB for BGRA). No CPU pixel arithmetic.

### Module Structure

Single file: `tauri-client/src-tauri/src/media/capture_wgc.rs`

Conditionally compiled with `#[cfg(target_os = "windows")]`, matching the existing `mod.rs` declaration.

## Components

### 1. Source Enumeration (`list_sources()`)

Returns `Vec<CaptureSource>` with both monitors and windows.

**Monitors:**
- `IDXGIFactory1::EnumAdapters1` to iterate GPU adapters
- `IDXGIAdapter1::EnumOutputs` to iterate monitor outputs per adapter
- `IDXGIOutput::GetDesc` for monitor name and desktop coordinates (width/height derived from `RECT`)
- ID format: `monitor:<adapter_idx>:<output_idx>`

**Windows:**
- `EnumWindows` callback iterating all top-level windows
- Filter: `IsWindowVisible`, non-zero title length, not minimized (`!IsIconic`), has `WS_CAPTION` style
- `GetWindowText` for window title
- `GetWindowRect` for dimensions
- ID format: `window:<hwnd_decimal>`

### 2. Capture Session (`start_capture()`)

**Setup (on blocking thread):**
1. Parse source ID to determine monitor or window target
2. Create D3D11 device with `D3D11_CREATE_DEVICE_BGRA_SUPPORT` flag (required for WGC interop)
3. Create `GraphicsCaptureItem` from HMONITOR or HWND via `CreateForMonitor` / `CreateForWindow` interop
4. Query source size from the capture item
5. Create D3D11 Video Processor (see section 3)
6. Create NV12 output texture and staging texture at target resolution
7. Create `Direct3D11CaptureFramePool` (pool size = 2, format = `B8G8R8A8UIntNormalized`)
8. Create `GraphicsCaptureSession` from the frame pool + capture item
9. Set `IsBorderRequired = false` (removes yellow capture border, requires Windows 11 or newer; gracefully ignored on Windows 10)
10. Start the session

**Capture thread:**
- Spawn a dedicated thread that runs a WinRT dispatcher/event loop
- `FrameArrived` callback on the frame pool:
  1. `TryGetNextFrame()` to acquire the BGRA frame
  2. Extract the `ID3D11Texture2D` from the WinRT `IDirect3DSurface`
  3. Run D3D11 Video Processor Blt (BGRA -> NV12)
  4. `CopyResource` NV12 texture to staging texture
  5. `Map` staging texture, copy NV12 data into `RawFrame`
  6. `Unmap` staging texture
  7. `try_send(frame)` on the channel:
     - `Ok(())` or `Full` -> continue (drop frame if full)
     - `Disconnected` -> signal capture loop to exit
- Listen for `GraphicsCaptureItem::Closed` event to handle source disappearing

### 3. D3D11 Video Processor

Performs GPU-side BGRA-to-NV12 conversion and optional scaling in a single `VideoProcessorBlt` call.

**One-time setup:**
1. `ID3D11Device::QueryInterface` -> `ID3D11VideoDevice`
2. `ID3D11DeviceContext::QueryInterface` -> `ID3D11VideoContext`
3. `CreateVideoProcessorEnumerator` with input (source size, BGRA) and output (target size, NV12)
4. `CreateVideoProcessor` from the enumerator
5. Create `ID3D11VideoProcessorInputView` (will be recreated per-frame since the BGRA texture changes)
6. Create `ID3D11VideoProcessorOutputView` on the persistent NV12 output texture

**Per-frame:**
1. Create input view for the current BGRA capture texture
2. Build `D3D11_VIDEO_PROCESSOR_STREAM` referencing the input view
3. Call `VideoProcessorBlt` (performs color conversion + scaling on GPU)

### 4. Staging & Readback

- NV12 output texture: `D3D11_USAGE_DEFAULT`, `D3D11_BIND_RENDER_TARGET` (video processor output)
- Staging texture: `D3D11_USAGE_STAGING`, `D3D11_CPU_ACCESS_READ`
- After `VideoProcessorBlt`, `CopyResource` from output to staging
- `Map` with `D3D11_MAP_READ`, copy Y plane then UV plane respecting stride, `Unmap`
- Output matches Linux format: contiguous `[Y plane][UV plane]` in `RawFrame.data`

### 5. Shutdown

Same pattern as Linux:
- Video pipeline drops `frame_rx` -> `try_send()` returns `Disconnected`
- Capture callback signals the event loop to exit
- `GraphicsCaptureSession` and frame pool are dropped (stops capture)
- D3D11 resources are released
- Capture thread exits

Also handles:
- `GraphicsCaptureItem::Closed` event (source disappeared) -> same exit path
- `on_window_event(CloseRequested)` in `lib.rs` already stops VideoEngine before app exit (implemented for Linux, works for Windows too)

## Error Handling

| Scenario | Handling |
|---|---|
| `D3D11CreateDevice` fails | Return error from `start_capture()` |
| No monitors/windows found | Return empty `Vec` from `list_sources()` |
| Video processor creation fails | Return error from `start_capture()` |
| Monitor/window disappears mid-capture | `Closed` event triggers clean shutdown |
| Frame pool starvation (GPU busy) | `FrameArrived` simply fires less often; natural backpressure |
| Channel full | Frame dropped silently (same as Linux) |
| Channel disconnected | Capture loop exits cleanly |

## Performance Characteristics

- **CPU:** Near-zero pixel processing. Only a single NV12 memcpy per frame (~3.1MB at 1080p, ~4.7MB at 1440p)
- **GPU:** D3D11 Video Processor uses dedicated hardware on most GPUs (not shader cores), minimal impact on game rendering
- **Memory:** 2 frame pool textures (BGRA) + 1 NV12 output + 1 NV12 staging = ~4 textures. At 1440p: ~52MB VRAM total
- **Latency:** Single-frame pipeline, no buffering beyond the 2-frame pool

## Platform Notes

- Requires Windows 10 version 1903+ (WGC API availability)
- `IsBorderRequired = false` requires Windows 11 (gracefully fails on Win10 -- yellow border shows)
- D3D11 Video Processor is available on all modern GPUs (NVIDIA, AMD, Intel)
- The `on_window_event` shutdown handler in `lib.rs` is platform-agnostic and already handles Windows
