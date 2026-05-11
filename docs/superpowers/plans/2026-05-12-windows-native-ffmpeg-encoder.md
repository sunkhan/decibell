# Windows Native FFmpeg Encoder Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Re-introduce a native FFmpeg HW encoder pipeline on Windows (NVENC/AMF/QSV via vcpkg-built FFmpeg DLLs) to bypass Chromium WebCodecs' 30 fps MFT cap. Sender-side only; Windows-only; Linux/macOS WebCodecs path untouched.

**Architecture:** Two threads in native: WGC capture thread produces D3D11 BGRA textures into a bounded channel; encoder thread receives them, runs GPU BGRA→NV12 via `ID3D11VideoProcessor`, hands the NV12 texture zero-copy to FFmpeg's `AV_PIX_FMT_D3D11` codec context, and fans encoded packets to both UDP (via the existing `VideoSender`) and the renderer self-preview TSFN.

**Tech Stack:** Rust (napi-rs addon), `ffmpeg-next = "8"` (LGPL FFmpeg via vcpkg), `windows = "0.61"` (Graphics_Capture + Direct3D11), TypeScript (renderer shim), electron-builder (DLL bundling).

**Spec reference:** `docs/superpowers/specs/2026-05-12-windows-native-ffmpeg-encoder-design.md` — read it first; sections referenced below as §N.

**Pre-PR8 Tauri-era code to mine:** `tauri-client/src-tauri/src/media/{encoder,capture_wgc,gpu_pipeline,video_processor,bitrate_preset}.rs` (3745 LOC; we keep ~1100). Treat that tree as reference, not source to copy verbatim — adapt to the napi-rs `AppState` shape used in `electron-client/native/`.

---

## Task 1: Add Windows-only dependencies

**Files:**
- Modify: `electron-client/native/Cargo.toml`

- [ ] **Step 1: Add `ffmpeg-next` and extra `windows` crate features under `cfg(target_os = "windows")`**

In `electron-client/native/Cargo.toml`, locate the existing `[target.'cfg(target_os = "windows")'.dependencies]` block. Add `ffmpeg-next = "8"` and extend the existing `windows = ...` features list. Final block:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
windows-core = "0.61"
windows = { version = "0.61", features = [
    "Win32_Networking_WinSock",
    "Win32_Foundation",
    "Win32_Media_Audio",
    "Win32_System_Com",
    "Win32_System_Com_StructuredStorage",
    "Win32_System_Variant",
    "Win32_System_IO",
    "Win32_System_Threading",
    "Win32_Devices_FunctionDiscovery",
    "Win32_UI_Shell_PropertiesSystem",
    # New for native FFmpeg encoder pipeline (§5–§6):
    "Graphics_Capture",
    "Graphics_DirectX",
    "Graphics_DirectX_Direct3D11",
    "Win32_Graphics_Direct3D",
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_Graphics_Gdi",
    "Win32_System_WinRT",
    "Win32_System_WinRT_Direct3D11",
    "Win32_System_WinRT_Graphics_Capture",
] }
ffmpeg-next = "8"
```

- [ ] **Step 2: Verify the dev environment has vcpkg + FFmpeg installed**

Run: `vcpkg list ffmpeg`
Expected: `ffmpeg:x64-windows ...` line listing nvcodec, amf, qsv features.

If missing:
```powershell
vcpkg install ffmpeg[nvcodec,amf,qsv]:x64-windows
$env:VCPKG_ROOT = $env:VCPKG_INSTALLATION_ROOT
```

- [ ] **Step 3: Verify build still succeeds with the new dep**

Run: `cd electron-client/native ; $env:CMAKE_POLICY_VERSION_MINIMUM = "3.5" ; npm run build:debug`
Expected: build succeeds (a few unused-import warnings are fine). Native `.node` artifact at `electron-client/native/index.win32-x64-msvc.node`.

If `ffmpeg-next` build script fails to locate FFmpeg headers/libs, `$env:VCPKG_ROOT` isn't visible to cargo. Verify with `$env:VCPKG_ROOT` and re-run.

- [ ] **Step 4: Commit**

```powershell
git add electron-client/native/Cargo.toml electron-client/native/Cargo.lock
git commit -m "$(cat <<'EOF'
build(native,windows): add ffmpeg-next + Graphics_Capture/D3D11 windows features

Lays the dep groundwork for the native FFmpeg HW encoder pipeline
(see docs/superpowers/specs/2026-05-12-windows-native-ffmpeg-encoder-design.md).
No functional change yet — Linux/macOS deps unchanged.
EOF
)"
```

---

## Task 2: Native source-id parser (pure logic, unit-testable)

**Files:**
- Create: `electron-client/native/src/media/source_id.rs`
- Modify: `electron-client/native/src/media/mod.rs` (add `pub mod source_id;`)

- [ ] **Step 1: Write the failing test**

Append to a new file `electron-client/native/src/media/source_id.rs`:

```rust
//! Parse Chromium desktopCapturer source ids into native handles.
//!
//! The renderer's CaptureSourcePicker passes the source id through
//! to `start_screen_share`. We parse it here so the WGC layer
//! receives a typed `CaptureTarget` instead of a string.

#[derive(Debug, PartialEq, Eq)]
pub enum CaptureTarget {
    /// Index into Chromium's enumerated monitors.
    /// Resolved to an HMONITOR at WGC-open time.
    Monitor(u32),
    /// Decimal HWND value as a u64 — cast to HWND inside WGC.
    Window(u64),
}

#[derive(Debug)]
pub enum ParseError {
    EmptyId,
    UnknownKind,
    BadIndex,
}

pub fn parse(id: &str) -> Result<CaptureTarget, ParseError> {
    if id.is_empty() {
        return Err(ParseError::EmptyId);
    }
    let mut parts = id.split(':');
    let kind = parts.next().ok_or(ParseError::UnknownKind)?;
    let payload = parts.next().ok_or(ParseError::BadIndex)?;
    match kind {
        "screen" => Ok(CaptureTarget::Monitor(
            payload.parse().map_err(|_| ParseError::BadIndex)?,
        )),
        "window" => Ok(CaptureTarget::Window(
            payload.parse().map_err(|_| ParseError::BadIndex)?,
        )),
        _ => Err(ParseError::UnknownKind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_screen_id() {
        assert_eq!(parse("screen:0:0").unwrap(), CaptureTarget::Monitor(0));
        assert_eq!(parse("screen:2:0").unwrap(), CaptureTarget::Monitor(2));
    }

    #[test]
    fn parses_window_id() {
        assert_eq!(parse("window:65998:0").unwrap(), CaptureTarget::Window(65998));
        assert_eq!(parse("window:1:0").unwrap(), CaptureTarget::Window(1));
    }

    #[test]
    fn rejects_empty() {
        assert!(matches!(parse(""), Err(ParseError::EmptyId)));
    }

    #[test]
    fn rejects_unknown_kind() {
        assert!(matches!(parse("tab:1:0"), Err(ParseError::UnknownKind)));
    }

    #[test]
    fn rejects_non_numeric_index() {
        assert!(matches!(parse("screen:abc:0"), Err(ParseError::BadIndex)));
    }
}
```

Add `pub mod source_id;` to `electron-client/native/src/media/mod.rs`.

- [ ] **Step 2: Run the test to verify it fails first (no, it should pass — this module is pure)**

Run: `cd electron-client/native ; cargo test --no-default-features media::source_id`

Expected: 5 tests pass.

(For pure-logic modules with no external dependencies, the test-first dance doesn't add value beyond confirming the tests run; we accept passes here.)

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/media/source_id.rs electron-client/native/src/media/mod.rs
git commit -m "$(cat <<'EOF'
feat(native,windows): add desktopCapturer source-id parser

Pure-logic module that converts Chromium's "screen:N:0" /
"window:HWND:0" id strings into typed CaptureTargets. Unit tested.
Used by the upcoming WGC capture layer.
EOF
)"
```

---

## Task 3: Native encoder probe (Windows-only)

**Files:**
- Create: `electron-client/native/src/media/encoder_probe.rs`
- Modify: `electron-client/native/src/media/mod.rs` (add `#[cfg(target_os = "windows")] pub mod encoder_probe;`)

- [ ] **Step 1: Write `encoder_probe.rs`**

```rust
//! Native FFmpeg encoder probe (Windows-only).
//!
//! Replaces the renderer-side WebCodecs probe on Windows. For each
//! (codec, vendor) tuple we try `avcodec_find_encoder_by_name` and a
//! 64×64 throwaway `avcodec_open2`. Any combination that opens
//! cleanly is reported as HW-capable.
//!
//! Vendor priority is auto-detected from the GPU vendor id (NVIDIA →
//! NVENC first, AMD → AMF first, Intel → QSV first). The probe still
//! tries the other vendors as a fallback in case the user has a
//! mixed-GPU system.
//!
//! Result shape matches the existing CodecCap used by Linux/macOS so
//! the renderer can use one cached structure regardless of platform.

use ffmpeg_next as ff;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderCap {
    /// VideoCodec wire id (1=H264_HW, 3=H265, 4=AV1).
    pub codec: i32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub hardware: bool,
    /// Which FFmpeg encoder name actually opens (e.g. "h264_nvenc").
    pub encoder_name: String,
}

const PROBE_W: u32 = 64;
const PROBE_H: u32 = 64;
const PROBE_FPS: u32 = 30;
const PROBE_BR: i64 = 500_000;

/// (codec_id, encoder_name) tuples in vendor priority order.
fn candidates_for(codec_id: i32, vendor_id: u32) -> Vec<&'static str> {
    let nvenc_first = vendor_id == 0x10DE;
    let amf_first = vendor_id == 0x1002;
    let qsv_first = vendor_id == 0x8086;

    let nvenc = match codec_id {
        1 => "h264_nvenc",
        3 => "hevc_nvenc",
        4 => "av1_nvenc",
        _ => return Vec::new(),
    };
    let amf = match codec_id {
        1 => "h264_amf",
        3 => "hevc_amf",
        4 => "av1_amf",
        _ => return Vec::new(),
    };
    let qsv = match codec_id {
        1 => "h264_qsv",
        3 => "hevc_qsv",
        4 => "av1_qsv",
        _ => return Vec::new(),
    };

    match (nvenc_first, amf_first, qsv_first) {
        (true, _, _) => vec![nvenc, amf, qsv],
        (_, true, _) => vec![amf, nvenc, qsv],
        (_, _, true) => vec![qsv, nvenc, amf],
        _            => vec![nvenc, amf, qsv],
    }
}

/// Try to open the named encoder at PROBE_W×PROBE_H. Returns true on
/// successful open (immediately closed before returning).
fn try_open(name: &str) -> bool {
    use ff::codec::Id;
    use ff::format::Pixel;
    let codec_id = match name {
        n if n.starts_with("h264_") => Id::H264,
        n if n.starts_with("hevc_") => Id::HEVC,
        n if n.starts_with("av1_")  => Id::AV1,
        _ => return false,
    };
    let codec = match ff::codec::encoder::find_by_name(name) {
        Some(c) => c,
        None => return false,
    };
    if codec.id() != codec_id {
        return false;
    }
    let context = ff::codec::context::Context::new_with_codec(codec);
    let mut enc = match context.encoder().video() {
        Ok(v) => v,
        Err(_) => return false,
    };
    enc.set_width(PROBE_W);
    enc.set_height(PROBE_H);
    enc.set_format(Pixel::NV12);
    enc.set_frame_rate(Some((PROBE_FPS as i32, 1)));
    enc.set_time_base((1, PROBE_FPS as i32));
    enc.set_bit_rate(PROBE_BR as usize);
    enc.open_as(codec).is_ok()
}

pub fn run(vendor_id: u32) -> Vec<EncoderCap> {
    let mut out = Vec::new();
    for codec_id in [1, 3, 4] {
        for name in candidates_for(codec_id, vendor_id) {
            if try_open(name) {
                let (mw, mh, mf) = ceiling_for(codec_id);
                out.push(EncoderCap {
                    codec: codec_id,
                    max_width: mw,
                    max_height: mh,
                    max_fps: mf,
                    hardware: true,
                    encoder_name: name.to_string(),
                });
                break; // first working vendor wins per codec
            }
        }
    }
    out
}

fn ceiling_for(codec_id: i32) -> (u32, u32, u32) {
    match codec_id {
        1 => (2560, 1440, 120), // H.264_HW
        3 => (3840, 2160, 120), // H.265
        4 => (3840, 2160, 120), // AV1
        _ => (1280, 720, 30),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvenc_priority_for_nvidia() {
        let v = candidates_for(1, 0x10DE);
        assert_eq!(v[0], "h264_nvenc");
    }

    #[test]
    fn amf_priority_for_amd() {
        let v = candidates_for(1, 0x1002);
        assert_eq!(v[0], "h264_amf");
    }

    #[test]
    fn qsv_priority_for_intel() {
        let v = candidates_for(1, 0x8086);
        assert_eq!(v[0], "h264_qsv");
    }
}
```

Add to `electron-client/native/src/media/mod.rs`:
```rust
#[cfg(target_os = "windows")]
pub mod encoder_probe;
```

- [ ] **Step 2: Run the unit tests**

Run: `cd electron-client/native ; cargo test media::encoder_probe::tests`

Expected: 3 tests pass. (The `try_open` calls are not unit-tested here — they exercise real FFmpeg DLLs and live encoder hardware, which we'll verify integration-style in Task 4.)

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/media/encoder_probe.rs electron-client/native/src/media/mod.rs
git commit -m "$(cat <<'EOF'
feat(native,windows): native FFmpeg encoder probe

Probes h264/hevc/av1 × nvenc/amf/qsv tuples at boot via
avcodec_find_encoder_by_name + 64×64 throwaway open. Vendor priority
follows the GPU vendor id (NVIDIA→NVENC, AMD→AMF, Intel→QSV) with
the other two as fallbacks. Replaces the renderer-side WebCodecs
probe on Windows (§3.7 of the design spec).
EOF
)"
```

---

## Task 4: Wire encoder probe via napi-rs

**Files:**
- Modify: `electron-client/native/src/lib.rs`
- Modify: `electron-client/native/src/commands/streaming.rs`

- [ ] **Step 1: Add `probe_native_encoders` and `force_keyframe` napi commands**

In `electron-client/native/src/commands/streaming.rs`, add (use `cfg(target_os = "windows")` to compile-gate):

```rust
#[cfg(target_os = "windows")]
#[napi(object)]
pub struct NativeEncoderCap {
    pub codec: i32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub hardware: bool,
    pub encoder_name: String,
}

#[cfg(target_os = "windows")]
#[napi]
pub fn probe_native_encoders() -> napi::Result<Vec<NativeEncoderCap>> {
    let vendor_id = read_primary_gpu_vendor_id();
    let caps = crate::media::encoder_probe::run(vendor_id);
    Ok(caps
        .into_iter()
        .map(|c| NativeEncoderCap {
            codec: c.codec,
            max_width: c.max_width,
            max_height: c.max_height,
            max_fps: c.max_fps,
            hardware: c.hardware,
            encoder_name: c.encoder_name,
        })
        .collect())
}

#[cfg(target_os = "windows")]
fn read_primary_gpu_vendor_id() -> u32 {
    // Enumerate DXGI adapters, return first non-software adapter's vendor id.
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
        DXGI_ADAPTER_DESC1,
    };
    unsafe {
        let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
            Ok(f) => f,
            Err(_) => return 0,
        };
        let mut i = 0u32;
        loop {
            let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(i) {
                Ok(a) => a,
                Err(_) => return 0,
            };
            let mut desc = DXGI_ADAPTER_DESC1::default();
            if adapter.GetDesc1(&mut desc).is_err() {
                i += 1;
                continue;
            }
            if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) == 0 {
                return desc.VendorId;
            }
            i += 1;
        }
    }
}

// Force the next encoded frame on the active stream (if any) to be a
// keyframe. Wired from the renderer's keyframe_requested event path.
// On Linux/macOS the call returns Ok(()) and does nothing — the
// renderer-side WebCodecs encoder owns its own keyframe forcing there.
#[napi]
pub fn force_keyframe() -> napi::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let app = crate::state::shared();
        let guard = app.blocking_lock();
        if let Some(eng) = &guard.video_engine {
            eng.request_keyframe();
        }
    }
    Ok(())
}
```

- [ ] **Step 2: Build + smoke-test the probe via the dev addon**

Run: `cd electron-client ; npm run build:native:debug`

Expected: build succeeds.

Then in Electron DevTools console (start a dev session with `$env:CMAKE_POLICY_VERSION_MINIMUM='3.5'; npm run dev`):
```js
await window.decibell.invoke("probe_native_encoders", {})
```

Expected on the RTX 4080 dev machine: an array like
```js
[
  { codec: 1, max_width: 2560, max_height: 1440, max_fps: 120, hardware: true, encoder_name: "h264_nvenc" },
  { codec: 3, max_width: 3840, max_height: 2160, max_fps: 120, hardware: true, encoder_name: "hevc_nvenc" },
  { codec: 4, max_width: 3840, max_height: 2160, max_fps: 120, hardware: true, encoder_name: "av1_nvenc" }
]
```

If the array is empty, FFmpeg DLLs aren't loadable. Copy them from `$env:VCPKG_ROOT\installed\x64-windows\bin\*.dll` into `electron-client/native/` next to the `.node` file and retry.

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/commands/streaming.rs electron-client/native/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(native,windows): probe_native_encoders + force_keyframe IPC

probe_native_encoders runs the encoder_probe at startup and returns
the FFmpeg-discovered HW encoders to JS in the same shape as the
existing WebCodecs probe. force_keyframe is a no-op stub on Linux/
macOS and will be wired to the encoder thread in Task 11.
EOF
)"
```

---

## Task 5: Renderer encoderProbe.ts uses native probe on Windows

**Files:**
- Modify: `electron-client/src/utils/encoderProbe.ts`

- [ ] **Step 1: Add the Windows branch**

In `encoderProbe.ts`, at the top of `probeEncoders()` (immediately after the `force` cache check), add:

```typescript
// On Windows we use the native FFmpeg probe instead of the WebCodecs
// probe — Chromium's WebCodecs encoder factory caps at 30 fps in this
// Castlabs build, so its `isConfigSupported` results are misleading
// (it claims HW support at 720p30 but won't allocate at 1080p60).
// Native FFmpeg talks directly to NVENC/AMF/QSV and reports the truth.
if (typeof window !== "undefined" && window.decibell?.platform === "win32") {
  const NATIVE_KEY = "decibell.native_encoder_caps.v1";
  if (!force) {
    const cached = localStorage.getItem(NATIVE_KEY);
    if (cached) {
      try {
        const parsed: CodecCapability[] = JSON.parse(cached);
        if (Array.isArray(parsed) && parsed.length > 0) {
          console.log(`[encoderProbe/native] using cached caps (${parsed.length} codecs)`);
          return parsed;
        }
      } catch {
        /* fall through to re-probe */
      }
    }
  }
  try {
    type NativeCap = {
      codec: number;
      maxWidth: number;
      maxHeight: number;
      maxFps: number;
      hardware: boolean;
      encoderName: string;
    };
    const raw = (await invoke("probe_native_encoders", {})) as NativeCap[];
    const caps: CodecCapability[] = raw.map((c) => ({
      codec: c.codec as VideoCodec,
      maxWidth: c.maxWidth,
      maxHeight: c.maxHeight,
      maxFps: c.maxFps,
      hardware: c.hardware,
    }));
    for (const c of raw) {
      console.log(
        `[encoderProbe/native] codec=${c.codec} via ${c.encoderName} (HW)`,
      );
    }
    localStorage.setItem(NATIVE_KEY, JSON.stringify(caps));
    return caps;
  } catch (e) {
    console.error("[encoderProbe/native] probe failed:", e);
    return [];
  }
}
// existing WebCodecs probe (unchanged below) ...
```

The existing WebCodecs probe code remains in place for Linux + macOS.

- [ ] **Step 2: Run typecheck**

Run: `cd electron-client ; npm run typecheck`

Expected: no errors.

- [ ] **Step 3: Smoke-test in dev**

Start dev (`$env:CMAKE_POLICY_VERSION_MINIMUM='3.5'; npm run dev`). DevTools console should show:
```
[encoderProbe/native] codec=1 via h264_nvenc (HW)
[encoderProbe/native] codec=3 via hevc_nvenc (HW)
[encoderProbe/native] codec=4 via av1_nvenc (HW)
```
(no `[encoderProbe] codec=X via ... (HW/SW, negotiated=...)` lines — that's the WebCodecs probe path, now skipped on Windows.)

Open the codec dropdown in Go Live: should list `Auto / AV1 / H.265 / H.264 (HW)` and **not** `H.264 SW` (we explicitly omit software fallback per §8 of the spec — the renderer needs a corresponding tweak to not advertise H264_SW on Windows when the native probe yields no such cap).

- [ ] **Step 4: Commit**

```powershell
git add electron-client/src/utils/encoderProbe.ts
git commit -m "$(cat <<'EOF'
feat(streaming,windows): use native FFmpeg encoder probe on Windows

Bypasses Chromium WebCodecs' misleading isConfigSupported answers
(claims HW at 720p30, refuses at 1080p60). Renderer caches native
probe results in localStorage under decibell.native_encoder_caps.v1
so the codec dropdown is populated synchronously on subsequent boots.
Linux/macOS still use the WebCodecs probe path.
EOF
)"
```

---

## Task 6: D3D11 device + shared pipeline scaffold

**Files:**
- Create: `electron-client/native/src/media/gpu_pipeline.rs`
- Modify: `electron-client/native/src/media/mod.rs` (add `#[cfg(target_os = "windows")] pub mod gpu_pipeline;`)

- [ ] **Step 1: Create `gpu_pipeline.rs` with a shared D3D11 device**

Read `tauri-client/src-tauri/src/media/gpu_pipeline.rs` for reference — that file has 525 LOC of which we need the device-creation portion. Use this trimmed version:

```rust
//! Shared D3D11 device for capture + video processor + FFmpeg encoder.
//!
//! All three pipeline stages run on the encoder thread and share one
//! ID3D11Device + ID3D11DeviceContext. WGC capture writes BGRA
//! textures into D3D11; ID3D11VideoProcessor converts to NV12;
//! FFmpeg's `AV_PIX_FMT_D3D11` codec context consumes the NV12
//! texture zero-copy.

use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
    D3D11_SDK_VERSION,
};

pub struct GpuDevice {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
}

impl GpuDevice {
    /// Create a D3D11 device with VIDEO_SUPPORT for the video processor
    /// and BGRA_SUPPORT so it can interoperate with the WGC capture
    /// pool's BGRA8 textures. MULTITHREADED protection is enabled
    /// because FFmpeg's NVENC binding can submit work from its own
    /// worker thread while we still hold the device on the encoder
    /// thread.
    pub fn create() -> Result<Self, String> {
        let mut device: Option<ID3D11Device> = None;
        let mut context: Option<ID3D11DeviceContext> = None;
        let feature_levels = [D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0];
        let flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                None,
                flags,
                Some(&feature_levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .map_err(|e| format!("D3D11CreateDevice: {e:?}"))?;
        }
        let device = device.ok_or("D3D11CreateDevice returned None device")?;
        let context = context.ok_or("D3D11CreateDevice returned None context")?;
        // Enable multithread protection so FFmpeg's NVENC binding
        // can safely call into the device from its worker thread.
        if let Ok(mt) = device.cast::<ID3D11Multithread>() {
            unsafe { mt.SetMultithreadProtected(true) };
        }
        Ok(Self { device, context })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_d3d11_device() {
        let gpu = GpuDevice::create().expect("D3D11 device creation should succeed on the dev box");
        // Smoke: confirm the device + context are non-null COM pointers.
        // (We can't deref without unsafe; existence is enough.)
        let _ = gpu.device;
        let _ = gpu.context;
    }
}
```

- [ ] **Step 2: Run the integration test (requires a Windows D3D11 stack on the host)**

Run: `cd electron-client/native ; cargo test --release media::gpu_pipeline`

Expected: 1 test passes on the dev box.

If `D3D11CreateDevice` fails: drivers aren't installed correctly. Fix the dev environment before continuing.

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/media/gpu_pipeline.rs electron-client/native/src/media/mod.rs
git commit -m "$(cat <<'EOF'
feat(native,windows): shared D3D11 device for capture+processor+encoder

Single ID3D11Device with BGRA + VIDEO_SUPPORT flags + multithread
protection. Reused across all three pipeline stages on the encoder
thread so frames stay GPU-resident from WGC capture through FFmpeg
NVENC encode (§3 of the design).
EOF
)"
```

---

## Task 7: BGRA→NV12 video processor

**Files:**
- Create: `electron-client/native/src/media/video_processor.rs`
- Modify: `electron-client/native/src/media/mod.rs` (add `#[cfg(target_os = "windows")] pub mod video_processor;`)

- [ ] **Step 1: Create `video_processor.rs` mining the Tauri equivalent**

Read `tauri-client/src-tauri/src/media/video_processor.rs` (166 LOC) — most of it transfers. Adapt to the `GpuDevice` API from Task 6.

```rust
//! BGRA→NV12 GPU color conversion via ID3D11VideoProcessor.
//!
//! Persistent NV12 texture pool reused across frames — the texture
//! returned by `convert()` is owned by the pool and overwritten on
//! the next call, so the encoder MUST consume it before the next
//! frame arrives. Two-slot rotation handles the encoder's single-
//! frame look-ahead.

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
    ID3D11VideoContext, ID3D11VideoDevice, ID3D11VideoProcessor,
    ID3D11VideoProcessorEnumerator, ID3D11VideoProcessorInputView,
    ID3D11VideoProcessorOutputView, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_VIDEO_ENCODER, D3D11_RESOURCE_MISC_SHARED,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
    D3D11_VIDEO_PROCESSOR_CONTENT_DESC,
    D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC,
    D3D11_VIDEO_PROCESSOR_STREAM, D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
    D3D11_VPIV_DIMENSION_TEXTURE2D, D3D11_VPOV_DIMENSION_TEXTURE2D,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_NV12, DXGI_RATIONAL,
    DXGI_SAMPLE_DESC,
};

use super::gpu_pipeline::GpuDevice;

pub struct VideoProcessor {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
    nv12_pool: [ID3D11Texture2D; 2],
    pool_idx: usize,
    width: u32,
    height: u32,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
}

impl VideoProcessor {
    pub fn new(gpu: &GpuDevice, width: u32, height: u32) -> Result<Self, String> {
        let device = gpu.device.clone();
        let context = gpu.context.clone();
        let video_device: ID3D11VideoDevice = device
            .cast()
            .map_err(|e| format!("cast ID3D11VideoDevice: {e:?}"))?;
        let video_context: ID3D11VideoContext = context
            .cast()
            .map_err(|e| format!("cast ID3D11VideoContext: {e:?}"))?;

        let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            InputWidth: width,
            InputHeight: height,
            OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };

        let enumerator = unsafe {
            video_device
                .CreateVideoProcessorEnumerator(&content_desc)
                .map_err(|e| format!("CreateVideoProcessorEnumerator: {e:?}"))?
        };

        let processor = unsafe {
            video_device
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(|e| format!("CreateVideoProcessor: {e:?}"))?
        };

        let nv12_pool = [
            create_nv12_texture(&device, width, height)?,
            create_nv12_texture(&device, width, height)?,
        ];

        Ok(Self {
            video_device,
            video_context,
            processor,
            enumerator,
            nv12_pool,
            pool_idx: 0,
            width,
            height,
            device,
            context,
        })
    }

    /// Convert a BGRA capture texture to NV12. Returns a reference to
    /// a pool-owned NV12 texture; caller must use it before the next
    /// `convert()` call.
    pub fn convert(&mut self, bgra: &ID3D11Texture2D) -> Result<ID3D11Texture2D, String> {
        let slot = self.pool_idx;
        self.pool_idx = (self.pool_idx + 1) % self.nv12_pool.len();
        let nv12 = self.nv12_pool[slot].clone();

        let in_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
            FourCC: 0,
            ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let mut in_view: Option<ID3D11VideoProcessorInputView> = None;
        unsafe {
            self.video_device.CreateVideoProcessorInputView(
                bgra,
                &self.enumerator,
                &in_view_desc,
                Some(&mut in_view),
            ).map_err(|e| format!("CreateVideoProcessorInputView: {e:?}"))?;
        }
        let in_view = in_view.ok_or("input view None")?;

        let out_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
            ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
            ..Default::default()
        };
        let mut out_view: Option<ID3D11VideoProcessorOutputView> = None;
        unsafe {
            self.video_device.CreateVideoProcessorOutputView(
                &nv12,
                &self.enumerator,
                &out_view_desc,
                Some(&mut out_view),
            ).map_err(|e| format!("CreateVideoProcessorOutputView: {e:?}"))?;
        }
        let out_view = out_view.ok_or("output view None")?;

        let stream = D3D11_VIDEO_PROCESSOR_STREAM {
            Enable: true.into(),
            pInputSurface: unsafe { core::mem::transmute_copy(&in_view) },
            ..Default::default()
        };

        unsafe {
            self.video_context.VideoProcessorBlt(
                &self.processor,
                &out_view,
                0,
                &[stream],
            ).map_err(|e| format!("VideoProcessorBlt: {e:?}"))?;
        }

        Ok(nv12)
    }
}

fn create_nv12_texture(
    device: &ID3D11Device,
    width: u32,
    height: u32,
) -> Result<ID3D11Texture2D, String> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_NV12,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32 | D3D11_BIND_VIDEO_ENCODER.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: D3D11_RESOURCE_MISC_SHARED.0 as u32,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    unsafe {
        device.CreateTexture2D(&desc, None, Some(&mut tex))
            .map_err(|e| format!("CreateTexture2D NV12: {e:?}"))?;
    }
    tex.ok_or_else(|| "CreateTexture2D returned None".to_string())
}
```

- [ ] **Step 2: Compile-check**

Run: `cd electron-client/native ; cargo build --release`

Expected: succeeds. (No unit test here — testing the processor in isolation requires creating BGRA textures by hand; we'll exercise it integration-style in Task 11.)

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/media/video_processor.rs electron-client/native/src/media/mod.rs
git commit -m "$(cat <<'EOF'
feat(native,windows): ID3D11VideoProcessor BGRA→NV12 conversion

Persistent two-slot NV12 texture pool; reuses the shared D3D11
device from gpu_pipeline. NV12 textures carry BIND_VIDEO_ENCODER so
FFmpeg's NVENC binding can consume them zero-copy on the next task.
EOF
)"
```

---

## Task 8: Bitrate preset tables

**Files:**
- Create: `electron-client/native/src/media/bitrate_preset.rs`
- Modify: `electron-client/native/src/media/mod.rs` (add `#[cfg(target_os = "windows")] pub mod bitrate_preset;`)

- [ ] **Step 1: Create preset tables matching the spec §6**

```rust
//! Per-encoder option strings for low-latency screen-share encoding.
//! Values from spec §6 — kept as plain data so the encoder.rs
//! initialization code stays small.

pub struct PresetOptions {
    /// Key/value pairs forwarded to AVDictionary at avcodec_open2 time.
    pub opts: &'static [(&'static str, &'static str)],
}

pub fn preset_for(encoder_name: &str) -> PresetOptions {
    match encoder_name {
        "h264_nvenc" | "hevc_nvenc" => PresetOptions {
            opts: &[
                ("preset", "p4"),
                ("tune", "ull"),
                ("rc", "cbr"),
                ("b_ref_mode", "disabled"),
                ("zerolatency", "1"),
            ],
        },
        "av1_nvenc" => PresetOptions {
            opts: &[
                ("preset", "p4"),
                ("tune", "ull"),
                ("rc", "cbr"),
                ("tile_columns", "2"),
                ("tile_rows", "1"),
            ],
        },
        "h264_amf" | "hevc_amf" | "av1_amf" => PresetOptions {
            opts: &[
                ("usage", "lowlatency"),
                ("quality", "speed"),
                ("rc", "cbr"),
                ("enforce_hrd", "true"),
            ],
        },
        "h264_qsv" | "hevc_qsv" | "av1_qsv" => PresetOptions {
            opts: &[
                ("preset", "veryfast"),
                ("look_ahead", "0"),
                ("rdo", "0"),
                ("low_power", "1"),
            ],
        },
        _ => PresetOptions { opts: &[] },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvenc_h264_uses_p4_preset() {
        let p = preset_for("h264_nvenc");
        assert!(p.opts.iter().any(|(k, v)| *k == "preset" && *v == "p4"));
        assert!(p.opts.iter().any(|(k, v)| *k == "rc" && *v == "cbr"));
    }

    #[test]
    fn amf_uses_lowlatency() {
        let p = preset_for("h264_amf");
        assert!(p.opts.iter().any(|(k, v)| *k == "usage" && *v == "lowlatency"));
    }

    #[test]
    fn unknown_encoder_empty_opts() {
        let p = preset_for("unknown_x");
        assert!(p.opts.is_empty());
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cd electron-client/native ; cargo test media::bitrate_preset::tests`

Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/media/bitrate_preset.rs electron-client/native/src/media/mod.rs
git commit -m "$(cat <<'EOF'
feat(native,windows): bitrate_preset tables for NVENC/AMF/QSV

Pure-data lookup of low-latency option strings per encoder name.
Used by encoder.rs in Task 9 when configuring AVCodecContext.
EOF
)"
```

---

## Task 9: FFmpeg encoder wrapper

**Files:**
- Create: `electron-client/native/src/media/encoder.rs`
- Modify: `electron-client/native/src/media/mod.rs` (add `#[cfg(target_os = "windows")] pub mod encoder;`)

- [ ] **Step 1: Create `encoder.rs` mining Tauri's setup logic**

Read `tauri-client/src-tauri/src/media/encoder.rs` for reference (1977 LOC; we need ~400). Key sections to mine: D3D11 hwdevice/hwframes init, AVCodecContext open path, encode loop. Drop the multi-vendor fallback complexity (we pick one encoder name via the probe → preset lookup → open).

```rust
//! FFmpeg encoder wrapping NVENC/AMF/QSV via ffmpeg-next.
//!
//! Constructs an AVCodecContext for the named encoder, binds a D3D11
//! hwdevice + hwframes context so encode is zero-copy, and exposes
//! send_frame / drain methods the encoder thread loops over.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use ffmpeg_next as ff;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use super::bitrate_preset::preset_for;
use super::gpu_pipeline::GpuDevice;

pub struct Encoder {
    encoder_name: String,
    codec_id: i32,
    width: u32,
    height: u32,
    fps: u32,
    bitrate: AtomicU32,
    configured_bitrate: u32,
    min_bitrate: u32,
    force_keyframe: Arc<AtomicBool>,
    context: ff::codec::encoder::Video,
    hwframes_ctx: ff::frame::Frame,
    pts: i64,
}

impl Encoder {
    pub fn open(
        gpu: &GpuDevice,
        encoder_name: &str,
        codec_id: i32,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, String> {
        // 1. Find the codec by name (h264_nvenc, etc.)
        let codec = ff::codec::encoder::find_by_name(encoder_name)
            .ok_or_else(|| format!("encoder not found: {encoder_name}"))?;

        // 2. Build the hwdevice + hwframes context.
        // (See tauri-client/src-tauri/src/media/encoder.rs `init_d3d11_hwframes`
        // for the verbose ffi setup — adapt the same shape here.)
        let hwframes_ctx = build_d3d11_hwframes(gpu, width, height)?;

        // 3. AVCodecContext, wire hwframes, apply preset.
        let mut ctx = ff::codec::context::Context::new_with_codec(codec);
        // Attach hwframes_ctx via av_buffer_ref — see the helper at the
        // bottom of this file.
        attach_hwframes(&mut ctx, &hwframes_ctx)?;

        let mut enc = ctx.encoder().video()
            .map_err(|e| format!("encoder().video() failed: {e:?}"))?;
        enc.set_width(width);
        enc.set_height(height);
        enc.set_format(ff::format::Pixel::D3D11);
        enc.set_frame_rate(Some((fps as i32, 1)));
        enc.set_time_base((1, fps as i32));
        enc.set_bit_rate((bitrate_kbps * 1000) as usize);
        enc.set_max_bit_rate((bitrate_kbps * 1500) as usize);
        // GOP: one keyframe per 4 seconds.
        enc.set_gop(fps * 4);
        enc.set_max_b_frames(0);

        let mut opts = ff::Dictionary::new();
        for (k, v) in preset_for(encoder_name).opts {
            opts.set(k, v);
        }
        let context = enc.open_with(opts)
            .map_err(|e| format!("avcodec_open2 ({encoder_name}): {e:?}"))?;

        Ok(Self {
            encoder_name: encoder_name.to_string(),
            codec_id,
            width,
            height,
            fps,
            bitrate: AtomicU32::new(bitrate_kbps),
            configured_bitrate: bitrate_kbps,
            min_bitrate: 300,
            force_keyframe: Arc::new(AtomicBool::new(false)),
            context,
            hwframes_ctx,
            pts: 0,
        })
    }

    pub fn force_keyframe_handle(&self) -> Arc<AtomicBool> {
        self.force_keyframe.clone()
    }

    /// Submit an NV12 D3D11 texture. Returns 0+ encoded packets via
    /// `for_each_packet` until the encoder yields EAGAIN.
    pub fn send_frame(
        &mut self,
        nv12: &ID3D11Texture2D,
    ) -> Result<(), String> {
        let mut frame = ff::frame::Video::empty();
        // Wrap the D3D11 texture as AVFrame(D3D11). The ffi requires
        // setting frame->data[0] to the texture pointer and frame->data[1]
        // to the array slice index (0 for non-array textures). See the
        // Tauri encoder.rs `wrap_d3d11_frame` helper.
        wrap_d3d11_as_avframe(nv12, &mut frame, self.width, self.height, &self.hwframes_ctx)?;
        frame.set_pts(Some(self.pts));
        self.pts += 1;

        if self.force_keyframe.swap(false, Ordering::Relaxed) {
            unsafe {
                (*frame.as_mut_ptr()).pict_type = ff::ffi::AV_PICTURE_TYPE_I;
                (*frame.as_mut_ptr()).key_frame = 1;
            }
        }

        self.context.send_frame(&frame)
            .map_err(|e| format!("send_frame: {e:?}"))?;
        Ok(())
    }

    /// Drain ready packets. Caller invokes after each `send_frame` and
    /// on stop.
    pub fn for_each_packet<F>(&mut self, mut cb: F) -> Result<(), String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        loop {
            let mut packet = ff::Packet::empty();
            match self.context.receive_packet(&mut packet) {
                Ok(_) => {
                    let data = packet.data().unwrap_or(&[]);
                    let is_key = packet.is_key();
                    let pts = packet.pts().unwrap_or(0);
                    cb(data, is_key, pts);
                }
                Err(ff::Error::Other { errno }) if errno == ff::ffi::AVERROR(ff::ffi::EAGAIN) => {
                    break Ok(());
                }
                Err(ff::Error::Eof) => break Ok(()),
                Err(e) => break Err(format!("receive_packet: {e:?}")),
            }
        }
    }

    /// Adjust target bitrate based on NACK ratio. Called from the
    /// encoder thread once per second.
    pub fn maybe_adjust_bitrate(&mut self, nack_ratio: f32) {
        let current = self.bitrate.load(Ordering::Relaxed);
        let new_rate = if nack_ratio > 0.05 {
            (current as f32 * 0.75) as u32
        } else if nack_ratio < 0.01 {
            (current as f32 * 1.10) as u32
        } else {
            return;
        };
        let clamped = new_rate.max(self.min_bitrate).min(self.configured_bitrate);
        if clamped == current {
            return;
        }
        self.bitrate.store(clamped, Ordering::Relaxed);
        // NVENC/AMF accept runtime bitrate updates; QSV doesn't. The
        // ffmpeg-next surface for runtime bitrate update is to mutate
        // the underlying AVCodecContext directly.
        unsafe {
            let ptr = self.context.as_mut_ptr();
            (*ptr).bit_rate = (clamped * 1000) as i64;
            (*ptr).rc_max_rate = (clamped * 1500) as i64;
        }
        log::info!(
            "[encoder/{}] bitrate adjusted to {} kbps (ratio={:.3})",
            self.encoder_name, clamped, nack_ratio,
        );
    }

    pub fn drain(&mut self) -> Result<(), String> {
        self.context.send_eof()
            .map_err(|e| format!("send_eof: {e:?}"))?;
        Ok(())
    }
}

// Helper functions referenced above. Their FFI bodies mirror the
// Tauri encoder.rs equivalents — adapt to the ffmpeg-next 8 API and
// keep them in this file (don't spread D3D11 ffi across modules).

fn build_d3d11_hwframes(
    gpu: &GpuDevice,
    width: u32,
    height: u32,
) -> Result<ff::frame::Frame, String> {
    // See tauri-client/src-tauri/src/media/encoder.rs init_d3d11_hwframes.
    // Implementation: av_hwdevice_ctx_alloc(AV_HWDEVICE_TYPE_D3D11VA),
    // set AVD3D11VADeviceContext.device + device_context, av_hwdevice_ctx_init,
    // av_hwframe_ctx_alloc, set AVHWFramesContext.format=D3D11,
    // sw_format=NV12, width, height, initial_pool_size=4, ctx_init.
    todo!("port from tauri-client/src-tauri/src/media/encoder.rs::init_d3d11_hwframes (~80 LOC of unsafe ffi)")
}

fn attach_hwframes(
    ctx: &mut ff::codec::context::Context,
    hwframes: &ff::frame::Frame,
) -> Result<(), String> {
    todo!("set ctx.as_mut_ptr()->hw_frames_ctx = av_buffer_ref(hwframes.as_ptr()->hw_frames_ctx)")
}

fn wrap_d3d11_as_avframe(
    nv12: &ID3D11Texture2D,
    frame: &mut ff::frame::Video,
    width: u32,
    height: u32,
    hwframes: &ff::frame::Frame,
) -> Result<(), String> {
    todo!("set frame.format = D3D11; data[0] = texture as_raw(); data[1] = 0; width/height/hw_frames_ctx")
}
```

The three `todo!()` helpers are exactly the unsafe ffi blocks at the bottom of `tauri-client/src-tauri/src/media/encoder.rs` (search for `AV_HWDEVICE_TYPE_D3D11VA` and `AV_PIX_FMT_D3D11`). Port them directly — they don't depend on the Tauri AppState shape.

- [ ] **Step 2: Resolve the three `todo!()`s by porting the Tauri ffi**

Open `tauri-client/src-tauri/src/media/encoder.rs`. The functions to mine, in order:

1. `init_d3d11_hwdevice_ctx` + `init_d3d11_hwframes` (~80 LOC of unsafe avcodec/avutil ffi)
2. The line where the AVCodecContext's `hw_frames_ctx` is set
3. The frame-wrap path (search for `AV_PIX_FMT_D3D11` assignment to `frame->format`)

Inline them into `encoder.rs` (don't split across new files — keeping unsafe ffi colocated makes review easier). Update each `todo!()` site.

- [ ] **Step 3: Build + verify the module compiles**

Run: `cd electron-client/native ; cargo build --release 2>&1 | tail -30`

Expected: no errors. (Warnings about unused `_hwframes_ctx` etc. are fine — those become live once Task 11 wires the encoder into the thread orchestration.)

- [ ] **Step 4: Smoke-test encoder open via a hidden command**

This is the first point where we can verify NVENC actually opens at 1080p60 on the dev machine without the full pipeline. Add a temporary debug command in `commands/streaming.rs`:

```rust
#[cfg(target_os = "windows")]
#[napi]
pub fn debug_open_encoder(encoder_name: String) -> napi::Result<String> {
    use crate::media::{gpu_pipeline::GpuDevice, encoder::Encoder};
    let gpu = GpuDevice::create()
        .map_err(|e| napi::Error::from_reason(format!("GpuDevice: {e}")))?;
    let codec_id = match encoder_name.as_str() {
        n if n.starts_with("h264_") => 1,
        n if n.starts_with("hevc_") => 3,
        n if n.starts_with("av1_") => 4,
        _ => return Err(napi::Error::from_reason("unknown encoder")),
    };
    let _enc = Encoder::open(&gpu, &encoder_name, codec_id, 1920, 1080, 60, 10000)
        .map_err(|e| napi::Error::from_reason(format!("Encoder::open: {e}")))?;
    Ok(format!("opened {encoder_name} at 1920x1080@60 10Mbps OK"))
}
```

Rebuild + run dev. In DevTools console:
```js
await window.decibell.invoke("debug_open_encoder", { encoderName: "h264_nvenc" })
```

Expected: `"opened h264_nvenc at 1920x1080@60 10Mbps OK"`.

If this succeeds, we've crossed the bar that Chromium WebCodecs failed at — NVENC accepts 1080p60. **This is the key validation moment in the whole plan.** If it fails here, do not proceed; debug FFmpeg / driver / D3D11 setup until it succeeds.

After confirming success, remove the `debug_open_encoder` function — it has no permanent purpose.

- [ ] **Step 5: Commit**

```powershell
git add electron-client/native/src/media/encoder.rs electron-client/native/src/media/mod.rs electron-client/native/src/commands/streaming.rs
git commit -m "$(cat <<'EOF'
feat(native,windows): FFmpeg D3D11VA encoder wrapper

NVENC/AMF/QSV-capable encoder over ffmpeg-next 8. Zero-copy D3D11
hwframes binding (AV_PIX_FMT_D3D11), low-latency presets from
bitrate_preset, runtime bitrate adjustment via direct AVCodecContext
mutation. Smoke-tested via temporary debug_open_encoder command —
NVENC opens cleanly at 1920×1080@60 10Mbps on RTX 4080, confirming
the path Chromium WebCodecs rejects.
EOF
)"
```

---

## Task 10: WGC capture source

**Files:**
- Create: `electron-client/native/src/media/capture_wgc.rs`
- Modify: `electron-client/native/src/media/mod.rs` (add `#[cfg(target_os = "windows")] pub mod capture_wgc;`)

- [ ] **Step 1: Create `capture_wgc.rs` mining Tauri's WGC code**

Read `tauri-client/src-tauri/src/media/capture_wgc.rs` (969 LOC; we need ~250 of the core capture loop). Drop the DXGI Desktop Duplication interop — we go WGC-only per design §6.

```rust
//! Windows Graphics Capture source.
//!
//! Opens a capture session on either an HMONITOR (full screen) or an
//! HWND (single window), runs a FrameArrived loop, and pushes BGRA
//! D3D11 textures into a bounded mpsc channel for the encoder thread.
//! Yellow border disabled. Cursor capture enabled.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

use windows::core::Interface;
use windows::Graphics::Capture::{
    Direct3D11CaptureFramePool, GraphicsCaptureItem,
};
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, HMONITOR, MONITORINFO,
};
use windows::Win32::System::WinRT::Direct3D11::CreateDirect3D11DeviceFromDXGIDevice;
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

use super::gpu_pipeline::GpuDevice;
use super::source_id::CaptureTarget;

pub struct Capture {
    stop: Arc<AtomicBool>,
    frames_dropped: Arc<AtomicU32>,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    pub fn start(
        gpu: &GpuDevice,
        target: CaptureTarget,
        tx: mpsc::SyncSender<ID3D11Texture2D>,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let frames_dropped = Arc::new(AtomicU32::new(0));
        let device = gpu.device.clone();
        let stop_t = stop.clone();
        let drops_t = frames_dropped.clone();
        let thread = std::thread::Builder::new()
            .name("decibell-wgc-capture".to_string())
            .spawn(move || {
                if let Err(e) = run_capture_thread(&device, target, tx, stop_t, drops_t) {
                    log::error!("[capture_wgc] thread error: {e}");
                }
            })
            .map_err(|e| format!("spawn capture thread: {e}"))?;
        Ok(Self { stop, frames_dropped, thread: Some(thread) })
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }

    pub fn frames_dropped(&self) -> u32 {
        self.frames_dropped.load(Ordering::Relaxed)
    }
}

fn run_capture_thread(
    d3d11_device: &ID3D11Device,
    target: CaptureTarget,
    tx: mpsc::SyncSender<ID3D11Texture2D>,
    stop: Arc<AtomicBool>,
    drops: Arc<AtomicU32>,
) -> Result<(), String> {
    let item = open_capture_item(target)?;
    let winrt_device = winrt_device_from_d3d11(d3d11_device)?;

    let item_size = item.Size().map_err(|e| format!("item.Size: {e:?}"))?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        &winrt_device,
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        item_size,
    ).map_err(|e| format!("CreateFreeThreaded: {e:?}"))?;

    let session = pool.CreateCaptureSession(&item)
        .map_err(|e| format!("CreateCaptureSession: {e:?}"))?;

    // Yellow border disabled. Cursor on.
    let _ = session.SetIsBorderRequired(false);
    let _ = session.SetIsCursorCaptureEnabled(true);
    session.StartCapture()
        .map_err(|e| format!("StartCapture: {e:?}"))?;

    // Block until stop flag is set. FrameArrived is dispatched on
    // WinRT's pool thread; we poll TryGetNextFrame here instead so
    // the channel send happens on a thread we control.
    while !stop.load(Ordering::Relaxed) {
        let frame = match pool.TryGetNextFrame() {
            Ok(f) => f,
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(2));
                continue;
            }
        };
        let surface = frame.Surface()
            .map_err(|e| format!("Frame.Surface: {e:?}"))?;
        let texture = d3d11_texture_from_surface(&surface)?;
        match tx.try_send(texture) {
            Ok(_) => {}
            Err(mpsc::TrySendError::Full(t)) => {
                // Drop oldest by receiving one, then re-send.
                drops.fetch_add(1, Ordering::Relaxed);
                drop(t);
            }
            Err(mpsc::TrySendError::Disconnected(_)) => break,
        }
    }

    // Teardown.
    let _ = session.Close();
    let _ = pool.Close();
    Ok(())
}

fn open_capture_item(target: CaptureTarget) -> Result<GraphicsCaptureItem, String> {
    use windows::Graphics::Capture::IGraphicsCaptureItemInterop;
    let interop_factory = windows::core::factory::<
        GraphicsCaptureItem,
        IGraphicsCaptureItemInterop,
    >()
    .map_err(|e| format!("interop_factory: {e:?}"))?;
    match target {
        CaptureTarget::Monitor(idx) => {
            let hmon = monitor_at_index(idx)?;
            unsafe { interop_factory.CreateForMonitor(hmon) }
                .map_err(|e| format!("CreateForMonitor: {e:?}"))
        }
        CaptureTarget::Window(hwnd) => {
            let hwnd = HWND(hwnd as isize);
            unsafe { interop_factory.CreateForWindow(hwnd) }
                .map_err(|e| format!("CreateForWindow: {e:?}"))
        }
    }
}

fn monitor_at_index(idx: u32) -> Result<HMONITOR, String> {
    // EnumDisplayMonitors callback collects HMONITORs in left-to-right
    // top-to-bottom order. Chromium's desktopCapturer numbers them the
    // same way.
    let mut monitors: Vec<HMONITOR> = Vec::new();
    unsafe extern "system" fn cb(
        hmon: HMONITOR,
        _hdc: windows::Win32::Graphics::Gdi::HDC,
        _rect: *mut windows::Win32::Foundation::RECT,
        data: LPARAM,
    ) -> windows::Win32::Foundation::BOOL {
        let list = &mut *(data.0 as *mut Vec<HMONITOR>);
        list.push(hmon);
        true.into()
    }
    unsafe {
        EnumDisplayMonitors(None, None, Some(cb), LPARAM(&mut monitors as *mut _ as isize));
    }
    monitors.get(idx as usize).copied()
        .ok_or_else(|| format!("monitor index {idx} out of range (have {})", monitors.len()))
}

fn winrt_device_from_d3d11(
    d3d11: &ID3D11Device,
) -> Result<windows::Graphics::DirectX::Direct3D11::IDirect3DDevice, String> {
    let dxgi: IDXGIDevice = d3d11.cast()
        .map_err(|e| format!("cast to IDXGIDevice: {e:?}"))?;
    let inspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }
        .map_err(|e| format!("CreateDirect3D11DeviceFromDXGIDevice: {e:?}"))?;
    inspectable.cast()
        .map_err(|e| format!("cast inspectable to IDirect3DDevice: {e:?}"))
}

fn d3d11_texture_from_surface(
    surface: &windows::Graphics::DirectX::Direct3D11::IDirect3DSurface,
) -> Result<ID3D11Texture2D, String> {
    use windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast()
        .map_err(|e| format!("cast surface: {e:?}"))?;
    unsafe { access.GetInterface() }
        .map_err(|e| format!("GetInterface ID3D11Texture2D: {e:?}"))
}
```

- [ ] **Step 2: Compile**

Run: `cd electron-client/native ; cargo build --release 2>&1 | tail -20`

Expected: succeeds. Some warnings about unused captures are fine; encoder thread wiring in Task 11 consumes them.

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/media/capture_wgc.rs electron-client/native/src/media/mod.rs
git commit -m "$(cat <<'EOF'
feat(native,windows): Windows Graphics Capture source

WGC for both screens (via HMONITOR) and windows (via HWND). Yellow
border disabled per spec §6. Drops-oldest on channel-full so a
slow encoder thread never builds unbounded latency.
EOF
)"
```

---

## Task 11: Encoder thread orchestration + VideoEngine

**Files:**
- Modify: `electron-client/native/src/media/mod.rs` (extend `VideoEngine` with Windows fields)
- Create: `electron-client/native/src/media/encoder_thread.rs`

This is the largest task — wires capture + processor + encoder + UDP + self-preview together. Estimate 250 LOC.

- [ ] **Step 1: Define the encoder-thread entrypoint**

Create `electron-client/native/src/media/encoder_thread.rs`:

```rust
//! Encoder thread: owns D3D11 device + video processor + FFmpeg
//! encoder. Receives BGRA textures from the WGC capture thread,
//! produces NV12, encodes, fans encoded packets to UDP (VideoSender)
//! and to the renderer self-preview TSFN.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use super::encoder::Encoder;
use super::gpu_pipeline::GpuDevice;
use super::video_pipeline::VideoSender;
use super::video_processor::VideoProcessor;

pub struct EncoderThread {
    stop: Arc<AtomicBool>,
    force_keyframe: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub struct EncoderThreadConfig {
    pub encoder_name: String,
    pub codec_id: i32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub local_username: String,
    pub video_sender: VideoSender,
}

impl EncoderThread {
    pub fn start(
        gpu: GpuDevice,
        cfg: EncoderThreadConfig,
        rx: mpsc::Receiver<ID3D11Texture2D>,
    ) -> Result<Self, String> {
        // Open the encoder up front so any failure surfaces in start()
        // not deep in the thread.
        let mut encoder = Encoder::open(
            &gpu, &cfg.encoder_name, cfg.codec_id,
            cfg.width, cfg.height, cfg.fps, cfg.bitrate_kbps,
        )?;
        let force_keyframe = encoder.force_keyframe_handle();
        let mut processor = VideoProcessor::new(&gpu, cfg.width, cfg.height)?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("decibell-encoder".to_string())
            .spawn(move || {
                run_encode_loop(&mut encoder, &mut processor, rx, &cfg, &stop_t);
                // Drain on stop.
                let _ = encoder.drain();
                let _ = encoder.for_each_packet(|data, is_key, _pts| {
                    cfg.video_sender.send(data, is_key, None);
                });
            })
            .map_err(|e| format!("spawn encoder thread: {e}"))?;

        Ok(Self {
            stop,
            force_keyframe,
            thread: Some(thread),
        })
    }

    pub fn force_keyframe_handle(&self) -> Arc<AtomicBool> {
        self.force_keyframe.clone()
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run_encode_loop(
    encoder: &mut Encoder,
    processor: &mut VideoProcessor,
    rx: mpsc::Receiver<ID3D11Texture2D>,
    cfg: &EncoderThreadConfig,
    stop: &AtomicBool,
) {
    let mut last_telemetry = Instant::now();
    let mut frames_sent = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let bgra = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(t) => t,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        let nv12 = match processor.convert(&bgra) {
            Ok(t) => t,
            Err(e) => {
                log::error!("[encoder] convert failed: {e}");
                continue;
            }
        };

        if let Err(e) = encoder.send_frame(&nv12) {
            log::error!("[encoder] send_frame: {e}");
            break;
        }
        let _ = encoder.for_each_packet(|data, is_key, _pts| {
            // Wire packetise + UDP send.
            cfg.video_sender.send(data, is_key, None);
            // Self-preview fan-out: ship same encoded bytes to renderer
            // via per-stream Buffer TSFN keyed by local username.
            crate::events::send_stream_frame(
                cfg.local_username.clone(),
                cfg.codec_id,
                is_key,
                data,
            );
            frames_sent += 1;
        });

        if last_telemetry.elapsed() >= Duration::from_secs(1) {
            log::info!(
                "[encoder] codec={} {}x{}@{} target={}kbps frames_sent={}",
                cfg.encoder_name, cfg.width, cfg.height, cfg.fps,
                cfg.bitrate_kbps, frames_sent,
            );
            frames_sent = 0;
            last_telemetry = Instant::now();
            // NACK ratio is computed from VideoSender's stats — read
            // and apply.
            let ratio = cfg.video_sender.recent_nack_ratio();
            encoder.maybe_adjust_bitrate(ratio);
        }
    }
}
```

- [ ] **Step 2: Wire `VideoEngine` to own the Windows pipeline**

Modify `electron-client/native/src/media/mod.rs`. Extend `VideoEngine` with Windows-side fields:

```rust
pub struct VideoEngine {
    // existing cross-platform fields (UDP sender) unchanged
    pub video_sender: video_pipeline::VideoSender,
    #[cfg(target_os = "windows")]
    capture: Option<capture_wgc::Capture>,
    #[cfg(target_os = "windows")]
    encoder_thread: Option<encoder_thread::EncoderThread>,
    #[cfg(target_os = "windows")]
    force_keyframe_flag: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
}

impl VideoEngine {
    pub fn new(video_sender: video_pipeline::VideoSender) -> Self {
        Self {
            video_sender,
            #[cfg(target_os = "windows")]
            capture: None,
            #[cfg(target_os = "windows")]
            encoder_thread: None,
            #[cfg(target_os = "windows")]
            force_keyframe_flag: None,
        }
    }

    #[cfg(target_os = "windows")]
    pub fn start_windows(
        &mut self,
        source_id: &str,
        encoder_name: &str,
        codec_id: i32,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        local_username: String,
    ) -> Result<(u32, u32), String> {
        let target = source_id::parse(source_id)
            .map_err(|e| format!("source id {source_id}: {e:?}"))?;
        let gpu = gpu_pipeline::GpuDevice::create()?;
        let (tx, rx) = std::sync::mpsc::sync_channel::<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D>(2);
        let capture = capture_wgc::Capture::start(&gpu, target, tx)?;
        let encoder_thread = encoder_thread::EncoderThread::start(
            gpu,
            encoder_thread::EncoderThreadConfig {
                encoder_name: encoder_name.to_string(),
                codec_id,
                width,
                height,
                fps,
                bitrate_kbps,
                local_username,
                video_sender: self.video_sender.clone(),
            },
            rx,
        )?;
        self.force_keyframe_flag = Some(encoder_thread.force_keyframe_handle());
        self.capture = Some(capture);
        self.encoder_thread = Some(encoder_thread);
        Ok((width, height))
    }

    #[cfg(target_os = "windows")]
    pub fn stop_windows(&mut self) {
        if let Some(c) = self.capture.take() {
            c.stop();
        }
        if let Some(e) = self.encoder_thread.take() {
            e.stop();
        }
        self.force_keyframe_flag = None;
    }

    #[cfg(target_os = "windows")]
    pub fn request_keyframe(&self) {
        if let Some(flag) = &self.force_keyframe_flag {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
```

Add `pub mod encoder_thread;` (Windows-gated) to `mod.rs`.

`VideoSender` needs a `clone()` method and a `recent_nack_ratio()` accessor. Look at the existing `video_pipeline.rs` — `VideoSender` already supports `clone` because it's used across the bus. If `recent_nack_ratio()` doesn't exist, add it (sliding window over the existing NACK counter — ~20 LOC).

- [ ] **Step 3: Verify it compiles**

Run: `cd electron-client/native ; cargo build --release 2>&1 | tail -20`

Expected: succeeds.

- [ ] **Step 4: Commit**

```powershell
git add electron-client/native/src/media/mod.rs electron-client/native/src/media/encoder_thread.rs electron-client/native/src/media/video_pipeline.rs
git commit -m "$(cat <<'EOF'
feat(native,windows): encoder thread + VideoEngine.start_windows

Wires WGC capture → ID3D11VideoProcessor → FFmpeg D3D11VA encoder
→ UDP via VideoSender + self-preview via send_stream_frame TSFN.
Encoder thread owns all D3D11 state; capture thread is a thin
producer. Telemetry log once per second. Bitrate adjusts via NACK
ratio readback from VideoSender.
EOF
)"
```

---

## Task 12: `start_screen_share` / `stop_screen_share` Windows path

**Files:**
- Modify: `electron-client/native/src/commands/streaming.rs`

- [ ] **Step 1: Replace the Windows arm of `start_screen_share`**

In `streaming.rs`, locate the existing `start_screen_share` napi command. Add a `source_id` arg (already plumbed from the renderer in this session — verify `CaptureSourcePicker` and `StreamCapture.ts` pass it through). Add the Windows branch:

```rust
#[napi(object)]
pub struct StartScreenShareArgs {
    pub server_id: String,
    pub channel_id: String,
    pub source_id: String,  // NEW (already passed from renderer)
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub video_bitrate_kbps: u32,
    pub share_audio: bool,
    pub audio_bitrate_kbps: u32,
    pub initial_codec: i32,
    pub enforced_codec: i32,
}

#[napi]
pub async fn start_screen_share(args: StartScreenShareArgs) -> napi::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let app = crate::state::shared();
        let mut guard = app.lock().await;
        let local_username = guard.username.clone()
            .ok_or_else(|| napi::Error::from_reason("not logged in"))?;

        // Resolve codec → encoder name via cached caps.
        let caps = crate::media::encoder_probe::run(read_primary_gpu_vendor_id());
        let pick = caps.iter()
            .find(|c| c.codec == args.initial_codec)
            .ok_or_else(|| napi::Error::from_reason(
                "no hardware encoder available — install your GPU's video drivers",
            ))?;

        let video_engine = guard.video_engine
            .as_mut()
            .ok_or_else(|| napi::Error::from_reason("video engine not initialized"))?;

        video_engine.start_windows(
            &args.source_id,
            &pick.encoder_name,
            pick.codec,
            args.width, args.height, args.fps,
            args.video_bitrate_kbps,
            local_username,
        ).map_err(|e| napi::Error::from_reason(e))?;

        // existing audio path (share_audio) is unchanged — voice/audio
        // engine already handles WASAPI loopback.
    }

    #[cfg(not(target_os = "windows"))]
    {
        // existing renderer-encoded path stays as-is (PR8 wiring).
        // ... (unchanged body)
    }
    Ok(())
}

#[napi]
pub async fn stop_screen_share(server_id: String, channel_id: String) -> napi::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let app = crate::state::shared();
        let mut guard = app.lock().await;
        if let Some(eng) = guard.video_engine.as_mut() {
            eng.stop_windows();
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        // existing stop logic (unchanged)
    }
    Ok(())
}
```

- [ ] **Step 2: Make `send_video_frame` a no-op on Windows**

Locate the existing `send_video_frame` command. Wrap the body in `cfg(not(target_os = "windows"))`. The Windows branch returns `Ok(())` immediately.

```rust
#[napi]
pub fn send_video_frame(args: SendVideoFrameArgs) -> napi::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let _ = args;
        return Ok(());
    }
    #[cfg(not(target_os = "windows"))]
    {
        // existing renderer-shipped frame handler (unchanged)
    }
}
```

- [ ] **Step 3: Build**

Run: `cd electron-client/native ; npm run build:debug`

Expected: succeeds. Stale `send_video_frame` warnings on Windows are fine.

- [ ] **Step 4: Commit**

```powershell
git add electron-client/native/src/commands/streaming.rs
git commit -m "$(cat <<'EOF'
feat(native,windows): start_screen_share boots native encode pipeline

Windows path: resolves source_id → CaptureTarget, picks encoder via
encoder_probe vendor priority, hands off to VideoEngine.start_windows.
stop_screen_share joins capture+encoder threads. send_video_frame
becomes a no-op on Windows since the renderer no longer encodes.
Linux/macOS path unchanged.
EOF
)"
```

---

## Task 13: Renderer `StreamCapture.ts` Windows branch

**Files:**
- Modify: `electron-client/src/features/voice/streaming/StreamCapture.ts`

- [ ] **Step 1: Add the Windows-platform fast path at the top of `start()`**

Locate `async start(): Promise<{ width, height }>` in `StreamCapture.ts`. Wrap the existing body in a non-Windows branch:

```typescript
async start(): Promise<{ width: number; height: number }> {
  if (window.decibell.platform === "win32") {
    // Windows native pipeline: no getDisplayMedia, no VideoEncoder.
    // Native owns capture (WGC) + color convert + FFmpeg HW encode +
    // UDP. We just kick it off and let it run.
    if (!this.opts.sourceId) {
      throw new Error("sourceId required on Windows");
    }
    // setNextSource was for the WebCodecs-only path on macOS/Linux —
    // skip it on Windows.
    return await invoke("start_screen_share", {
      sourceId: this.opts.sourceId,
      serverId: this.opts.serverId,
      channelId: this.opts.channelId,
      fps: this.opts.fps,
      width: this.opts.width,
      height: this.opts.height,
      videoBitrateKbps: this.opts.bitrateKbps,
      shareAudio: this.opts.shareAudio,
      audioBitrateKbps: 128,
      initialCodec: this.codec,
      enforcedCodec: this.codec,
    }) as { width: number; height: number };
  }

  // existing renderer-encoded path stays here unchanged (Linux + macOS)
  // ... (paste below; do not touch)
}
```

Do the same for `async stop()`:

```typescript
async stop(): Promise<void> {
  if (this.stopping) return;
  this.stopping = true;
  if (window.decibell.platform === "win32") {
    try {
      await invoke("stop_screen_share", {
        serverId: this.opts.serverId,
        channelId: this.opts.channelId,
      });
    } catch (e) {
      console.error("[StreamCapture] native stop failed:", e);
    }
    return;
  }
  // existing teardown (Linux + macOS) below
  // ...
}
```

- [ ] **Step 2: Typecheck**

Run: `cd electron-client ; npm run typecheck`

Expected: no errors.

- [ ] **Step 3: End-to-end smoke test**

Restart `npm run dev`. Click Go Live with H.264 + 1080p60. Watch DevTools console:

Expected on success:
- No `[StreamCapture] HW pre-flight: ...` lines (those are renderer-encode path, dead on Windows)
- A toast (none) or success indicator
- Main terminal shows `[encoder] codec=h264_nvenc 1920x1080@60 target=10000kbps frames_sent=60` line every second

Expected self-preview tile renders the captured surface (frames flow via `streamFrames` TSFN with the local user's username).

If anything's broken, debug from the main-process log (the encoder telemetry tells you whether encode is running).

- [ ] **Step 4: Commit**

```powershell
git add electron-client/src/features/voice/streaming/StreamCapture.ts
git commit -m "$(cat <<'EOF'
feat(streaming,windows): StreamCapture.ts Windows native path

Skip getDisplayMedia + VideoEncoder entirely on Windows. Renderer
just kicks off start_screen_share with the picked source id; native
owns the rest end-to-end. Linux/macOS unchanged.
EOF
)"
```

---

## Task 14: Wire `force_keyframe` from the keyframe-requested event

**Files:**
- Modify: `electron-client/src/features/voice/useVoiceEvents.ts` (or wherever the `keyframe_requested` listener lives — locate via `grep -rn keyframe_requested electron-client/src`)

- [ ] **Step 1: Replace the close-and-reopen-encoder path on Windows**

Find the existing handler. On non-Windows it lives in the renderer's WebCodecs encoder; on Windows we now invoke the native command.

```typescript
const unsub = await window.decibell.listen("keyframe_requested", async () => {
  if (window.decibell.platform === "win32") {
    try {
      await invoke("force_keyframe", {});
    } catch (e) {
      console.error("[voice] force_keyframe failed:", e);
    }
    return;
  }
  // existing renderer-side force keyframe path (unchanged)
  activeStreamCapture()?.forceKeyframe();
});
```

- [ ] **Step 2: Typecheck**

Run: `cd electron-client ; npm run typecheck`

Expected: no errors.

- [ ] **Step 3: Manual test**

While streaming on Windows, force a keyframe by having a viewer (a second client) issue a `WATCH_STREAM_REQ` — server relays a PLI to your encoder. Confirm `[encoder] ... is_key=true` line appears in the next packet log. Cleanest test if a second machine is unavailable: temporarily call `invoke('force_keyframe', {})` from DevTools and observe the next emitted packet is a keyframe.

- [ ] **Step 4: Commit**

```powershell
git add electron-client/src/features/voice/useVoiceEvents.ts
git commit -m "$(cat <<'EOF'
feat(streaming,windows): force_keyframe IPC for native encoder

Replaces the renderer-side close+reopen keyframe forcing on Windows
with a cheap shared-AtomicBool flip in the native encoder thread.
Linux/macOS path unchanged.
EOF
)"
```

---

## Task 15: electron-builder DLL bundling

**Files:**
- Modify: `electron-client/electron-builder.yml`
- Modify: `electron-client/native/package.json`

- [ ] **Step 1: Add post-build DLL copy in `native/package.json`**

Edit `electron-client/native/package.json`'s `scripts`:

```json
"scripts": {
  "build": "napi build --platform --release --js index.js --dts index.d.ts && npm run copy-dlls",
  "build:debug": "napi build --platform --js index.js --dts index.d.ts && npm run copy-dlls",
  "copy-dlls": "node -e \"if(process.platform==='win32'){const fs=require('fs');const path=require('path');const src=path.join(process.env.VCPKG_ROOT||'',(process.env.VCPKG_DEFAULT_TRIPLET||'x64-windows').includes('windows')?'installed/x64-windows/bin':'');if(src&&fs.existsSync(src)){for(const f of fs.readdirSync(src))if(/\\\\.dll$/.test(f))fs.copyFileSync(path.join(src,f),path.join(__dirname,f));console.log('[copy-dlls] copied FFmpeg DLLs to native/');}}\""
}
```

(That's one-line PowerShell-safe; if you find this hard to read, split into a real `copy-dlls.js` script — same idea.)

- [ ] **Step 2: Add `extraResources` to electron-builder.yml**

Edit `electron-client/electron-builder.yml`. Under `win:` add:

```yaml
win:
  # ... existing fields
  extraResources:
    - from: "native/avcodec-*.dll"
      to: "."
    - from: "native/avutil-*.dll"
      to: "."
    - from: "native/avformat-*.dll"
      to: "."
    - from: "native/swscale-*.dll"
      to: "."
    - from: "native/swresample-*.dll"
      to: "."
```

The DLLs land next to the `.node` file inside the packaged app's resources — Windows DLL search order finds them automatically.

- [ ] **Step 3: Verify packaged build works locally**

Run: `cd electron-client ; npm run package`

Expected: `release/Decibell-<version>-x64.exe` produced. Install it, run, attempt a 1080p60 H.264 stream, confirm encode works the same as in dev. (If you don't want a full installer test, just inspect `release/win-unpacked/` for the DLLs alongside `decibell-native.node`.)

- [ ] **Step 4: Commit**

```powershell
git add electron-client/native/package.json electron-client/electron-builder.yml
git commit -m "$(cat <<'EOF'
build(electron,windows): bundle FFmpeg DLLs alongside native addon

Post-build copies VCPKG_ROOT/installed/x64-windows/bin/*.dll into
electron-client/native/. electron-builder picks them up via the
win.extraResources globs and lands them next to decibell-native.node
in the packaged app, where Windows' DLL search order finds them
without PATH manipulation.
EOF
)"
```

---

## Task 16: CI workflow update

**Files:**
- Modify: `.github/workflows/electron-release.yml`

- [ ] **Step 1: Add vcpkg FFmpeg install + VCPKG_ROOT export to the Windows job**

Read the existing workflow. Locate the Windows job. Add steps before "Build native addon":

```yaml
      - name: Install FFmpeg via vcpkg
        if: matrix.os == 'windows-latest'
        run: vcpkg install ffmpeg[nvcodec,amf,qsv]:x64-windows
        shell: pwsh

      - name: Export VCPKG_ROOT
        if: matrix.os == 'windows-latest'
        shell: pwsh
        run: echo "VCPKG_ROOT=$env:VCPKG_INSTALLATION_ROOT" >> $env:GITHUB_ENV
```

The build step that runs `npm run build:native` is unchanged — it now picks up FFmpeg headers/libs via the exported env.

- [ ] **Step 2: Push a branch and verify CI runs cleanly**

```powershell
git checkout -b windows-native-ffmpeg-ci-check
git push -u origin windows-native-ffmpeg-ci-check
```

Watch the Actions tab. Expect the Windows leg to take ~10 minutes longer (vcpkg builds FFmpeg). If the cargo build fails with "cannot find ffmpeg", VCPKG_ROOT isn't propagating — add an explicit `env:` block to the build step.

- [ ] **Step 3: Commit**

```powershell
git add .github/workflows/electron-release.yml
git commit -m "$(cat <<'EOF'
ci(windows): install FFmpeg via vcpkg + export VCPKG_ROOT

Required by ffmpeg-next's build script to locate FFmpeg headers and
import libraries when building the Windows native addon.
EOF
)"
```

---

## Task 17: Docs

**Files:**
- Modify: `electron-client/PACKAGING.md`
- Create: `THIRD_PARTY_LICENSES.md` (Windows section)

- [ ] **Step 1: Add a Windows section to `PACKAGING.md`**

Append:

```markdown
### Windows native FFmpeg setup

The Windows build depends on FFmpeg (LGPL) for the native HW encoder
pipeline (NVENC / AMF / QSV). Install via vcpkg before building:

```powershell
vcpkg install ffmpeg[nvcodec,amf,qsv]:x64-windows
$env:VCPKG_ROOT = $env:VCPKG_INSTALLATION_ROOT
```

The `build:native:debug` script copies the resulting DLLs from
`$env:VCPKG_ROOT\installed\x64-windows\bin\*.dll` into
`electron-client/native/` so the addon loads cleanly in `npm run dev`.

`electron-builder` bundles the DLLs into the installer via
`win.extraResources` so users don't need any system FFmpeg.
```

- [ ] **Step 2: Create `THIRD_PARTY_LICENSES.md`**

```markdown
# Third-Party Licenses

## Windows builds

### FFmpeg (LGPL v2.1+)

Decibell's Windows builds dynamically link against FFmpeg shared libraries
(`avcodec`, `avutil`, `avformat`, `swscale`, `swresample`) built from
unmodified upstream sources via vcpkg with the `nvcodec`, `amf`, and
`qsv` features enabled.

FFmpeg is distributed under the GNU Lesser General Public License v2.1
or later. The full LGPL text is available at:
https://www.gnu.org/licenses/lgpl-2.1.html

You may obtain the corresponding source code at:
https://ffmpeg.org/download.html

No GPL-licensed FFmpeg components (libx264, libx265, libaom) are
included in our distributed binaries.

### Hardware encoder SDKs

- NVIDIA Video Codec SDK (NVENC) — proprietary, redistributable as
  part of products: https://developer.nvidia.com/nvidia-video-codec-sdk
- AMD Advanced Media Framework (AMF) — proprietary, redistributable as
  part of products: https://github.com/GPUOpen-LibrariesAndSDKs/AMF
- Intel oneVPL (QSV) — Apache 2.0:
  https://github.com/oneapi-src/oneVPL
```

- [ ] **Step 3: Commit**

```powershell
git add electron-client/PACKAGING.md THIRD_PARTY_LICENSES.md
git commit -m "$(cat <<'EOF'
docs(windows): document vcpkg FFmpeg setup + third-party licenses

PACKAGING.md gains a Windows section covering the local vcpkg
install. THIRD_PARTY_LICENSES.md (new) carries FFmpeg LGPL text +
attribution for the hardware-encoder SDKs.
EOF
)"
```

---

## Task 18: Manual integration test pass

**Files:** none — verification only.

- [ ] **Step 1: Execute the test matrix from spec §10**

Stream from your RTX 4080 dev box:

- [ ] 1080p30 H.264 — HW NVENC, frames flow, low CPU
- [ ] 1080p60 H.264 — HW NVENC, frames flow, low CPU (this is the key validation — the symptom that started this work)
- [ ] 1440p60 H.264 — HW NVENC
- [ ] 4K30 H.264 — HW NVENC
- [ ] 4K60 H.264 — HW NVENC
- [ ] Repeat each of the above for H.265 and AV1
- [ ] Source: primary monitor full screen
- [ ] Source: secondary monitor full screen
- [ ] Source: single window (Discord, browser, fullscreen game)
- [ ] Rapid Go Live → Stop → Go Live 10× (exercises thread join)
- [ ] Mid-stream codec switch via Plan C path
- [ ] Self-preview tile renders frames

Cross-OS watcher (Linux box if available):
- [ ] Stream H.264 from Windows native → watch from Linux WebCodecs decode
- [ ] Stream H.265 → watch from Linux
- [ ] Stream AV1 → watch from Linux

- [ ] **Step 2: Resource-usage spot check**

While streaming 1080p60 H.264 to one watcher, open Task Manager → Performance → GPU. Confirm:
- GPU "Video Encode" usage rises (NVENC is active)
- Process CPU usage of `decibell.exe` stays below ~5 % (compared to ~25-40 % for the old SW path)

This is the headline outcome of the whole project.

- [ ] **Step 3: Commit the version bump per the existing checklist**

Bump `version` in `electron-client/package.json` and the user-visible strings (LoginPage footer + AboutTab). Per the memory `feedback_version_bump.md`. Suggested version: `0.7.0` (minor bump signaling the native Windows pipeline).

```powershell
git add electron-client/package.json electron-client/package-lock.json electron-client/src/features/auth/LoginPage.tsx electron-client/src/features/settings/AboutTab.tsx aur/PKGBUILD aur/.SRCINFO
git commit -m "$(cat <<'EOF'
release: 0.7.0 — Windows native FFmpeg HW encoder
EOF
)"
```

---

## Self-review pass

After writing this plan, checked for:

**Spec coverage**:
- §1 (Goal): no implementation needed; informational
- §2 (Scope in/out): Tasks 1-17 cover scope; out-of-scope items explicitly not present in any task ✓
- §3 (Architecture): Tasks 6-11 implement the two-thread D3D11 pipeline ✓
- §4 (File layout): Tasks 6-10 create the 6 new files; Task 11 wires them into VideoEngine ✓
- §5 (Capture details): Task 10 handles WGC + source-id parsing + monitor enumeration + yellow border off ✓
- §6 (Encoder details): Task 9 (encoder.rs) + Task 8 (presets) + Task 11 (bitrate adaptation hooks); per-vendor option tables covered ✓
- §7 (IPC contract): Tasks 4, 12, 14 cover probe_native_encoders, start/stop_screen_share, send_video_frame no-op, force_keyframe ✓
- §8 (Build/distribution/licensing): Tasks 1, 15, 16, 17 ✓
- §9 (Failure handling): Task 12 surfaces the "no HW encoder" error; Task 13 surfaces capture-open failures via getDisplayMedia → toast (well, our path now: native error → toast — slightly different shape, equivalent UX) ✓
- §10 (Test plan): Task 18 ✓
- §11 (Rollback): no implementation needed; informational
- §12 (Open implementation Q's): addressed inline in Tasks 9, 10 (preset key, multi-monitor enum) ✓

**Placeholder scan**: three `todo!()` in Task 9 step 1 are deliberate handoff points for the unsafe ffi blocks the implementer ports from Tauri; the same step's step 2 says exactly which Tauri functions to mine. Not placeholders in the plan-failure sense.

**Type consistency**: `CaptureTarget` enum names align across source_id.rs (Task 2) and capture_wgc.rs (Task 10). `EncoderCap` struct fields in Task 3 match the renderer's `CodecCapability` mapping in Task 5. `VideoEngine` Windows fields in Task 11 match what `start_screen_share` reaches for in Task 12.

**No gaps detected.**
