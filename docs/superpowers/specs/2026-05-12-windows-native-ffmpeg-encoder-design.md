# Windows Native FFmpeg Encoder — Design Spec

**Date:** 2026-05-12
**Status:** Approved for implementation planning
**Scope:** Sender-side video pipeline on Windows. Re-introduces native FFmpeg HW encoding (NVENC + AMF + QSV) to bypass Chromium WebCodecs' 30 fps cap on Windows in Electron 33. Linux, macOS, and the receiver side are unchanged.

---

## 1. Goal

Empirical testing showed that this Castlabs Electron 33 build's Chromium has a uniform 30 fps cap on every WebCodecs HW video encoder. All eight `isConfigSupported` probes return `supported: false` at 1080p60 across H.264 / H.265 / AV1 with both `realtime` and default latency modes, even though:

- Boot probe confirms NVENC/HEVC/AV1 HW *exists* at 720p30 (`negotiated=prefer-hardware`).
- `app.getGPUFeatureStatus()` reports `video_encode: 'enabled'`.
- 1080p30 H.264 works in HW (`avc1.640028`).
- The same RTX 4080 NVENC silicon trivially handles 4K60+ via FFmpeg or OBS.

The cap is in Chromium's `MediaFoundationVideoEncoder` factory; we cannot fix it from JS or feature flags. Decibell's target audience is gamers, where 60 fps streaming with low CPU usage is the whole point. Software encode at 1080p60 (the current fallback) burns the CPU the game needs.

This spec re-introduces a native FFmpeg encoder under `#[cfg(target_os = "windows")]` — the Tauri-era code path that was deleted in PR8 because of Linux allocator conflicts with PipeWire/cpal. Those conflicts don't exist on Windows.

The Linux WebCodecs+VAAPI path keeps working; macOS WebCodecs+VideoToolbox keeps working. Only the Windows encoder pipeline changes.

---

## 2. Scope

**In scope:**
- Native Windows Graphics Capture (WGC) source for both screens and windows. Yellow capture border disabled. DXGI Desktop Duplication is not revived.
- D3D11 device + video processor for BGRA→NV12 GPU color conversion (single shared device).
- FFmpeg encoder wrapper supporting `h264_nvenc / hevc_nvenc / av1_nvenc / h264_amf / hevc_amf / av1_amf / h264_qsv / hevc_qsv / av1_qsv` with vendor auto-detection at boot.
- Native encoder probe replacing the WebCodecs probe on Windows; renderer reads caps via a new IPC command.
- Renderer `StreamCapture.ts` Windows branch: skip `getDisplayMedia` and `VideoEncoder`, call `start_screen_share` with the picked source id, let native do everything.
- Self-preview: native encoder fans encoded frames through the existing per-stream Buffer TSFN with the local user's username; renderer's `StreamVideoPlayer` decodes via its existing WebCodecs subscription. The `subscribeLocalFrames` channel becomes dead code on Windows.
- Keyframe forcing via new `force_keyframe` native command (replaces the wasteful close-and-reopen the renderer currently uses).
- Runtime bitrate adaptation in native, mirroring the WebCodecs path's NACK-ratio-driven `applyBitrate`.
- electron-builder bundles FFmpeg DLLs alongside the `.node` file.
- CI installs FFmpeg via vcpkg on the Windows runner.

**Out of scope (deferred):**
- Extending the WebCodecs **decoder** probe to verify 4K60 HW decode on Windows. Tracked as follow-up; D3D11VA decode is mature in Chromium and likely fine, but unverified.
- Linux / macOS native paths. WebCodecs is the path on both and works.
- Software fallback encoders inside the native path. We do not link `libx264` / `libx265` / `libaom` (all are GPL or licensing-awkward). If all hardware encoders fail to probe on a given Windows machine, native returns empty caps and the renderer shows "No hardware encoder available — install your GPU's video drivers". Every Windows GPU shipped since ~2012 has one of NVENC/AMF/QSV.
- Hot-swap encoder vendor when GPU is added/removed at runtime.
- Changes to the wire format, the codec negotiation logic, or the receiver-side decoder pipeline.

---

## 3. Architecture

```
JS renderer (CaptureSourcePicker)
    │
    │ sourceId (Chromium desktopCapturer id, "screen:N:0" or "window:HWND:0")
    ▼
invoke("start_screen_share", { sourceId, fps, bitrate, codec, ... })
    │
    ▼
─────────────────────────────────  native crate (Windows)  ──────────────────────────────────

  Capture thread                       Encoder thread
  (WinRT-owned via WGC pool)           (owns D3D11 device, video processor, FFmpeg ctx)
       │
       │ FrameArrived event
       │
       ▼ ID3D11Texture2D (BGRA)
  mpsc::Sender (bounded depth=2,        ◄── recv ──┐
                drop-oldest)                       │
       │                                           │
       └────────────────────────────────────────►─┘
                                                   │
                                                   ▼ ID3D11VideoProcessor::VideoProcessorBlt
                                              NV12 D3D11 texture (pool-allocated)
                                                   │
                                                   ▼ wrap as AVFrame(AV_PIX_FMT_D3D11)
                                              avcodec_send_frame
                                                   │
                                                   ▼ avcodec_receive_packet (loop)
                                              Encoded chunk (CPU)
                                                   │
                                ┌──────────────────┴─────────────────┐
                                ▼                                    ▼
                  VideoSender::send (existing)            events::send_stream_frame TSFN
                  (packetize + FEC + UDP)                 (local username, self-preview)
```

Two threads. Capture thread does only what WGC's pump requires; encoder thread holds all D3D11 + FFmpeg state on a single OS thread so the D3D11 device doesn't need multi-thread protection.

True zero-copy on NVENC/AMF/QSV: FFmpeg accepts the D3D11 NV12 texture directly via `AV_PIX_FMT_D3D11`. No CPU readback between capture and encode. Only the compressed bitstream returns to CPU memory for UDP packetization.

---

## 4. File layout

**New files** (all under `#[cfg(target_os = "windows")]`):

| File | Role | Source |
|------|------|--------|
| `native/src/media/capture_wgc.rs` | WGC session: `IGraphicsCaptureItem` from HWND or HMONITOR, `Direct3D11CaptureFramePool`, FrameArrived loop, mpsc sender. `IsBorderRequired = false`. | new (Tauri-era reference simpler now that DXGI is dropped) |
| `native/src/media/gpu_pipeline.rs` | Single D3D11 device shared across capture + processor + encoder. Provides handles for FFmpeg's `IMFDXGIDeviceManager` binding. | mined from `tauri-client/src-tauri/src/media/gpu_pipeline.rs` |
| `native/src/media/video_processor.rs` | `ID3D11VideoProcessor` BGRA→NV12 GPU color conversion with persistent NV12 texture pool. | mined from `tauri-client/.../video_processor.rs` |
| `native/src/media/encoder.rs` | FFmpeg encoder wrapper: probes vendor in priority order, opens AVCodecContext with D3D11 hwframes_ctx, runs the encode loop in the encoder thread, emits encoded packets. | mined from `tauri-client/.../encoder.rs`, trimmed to current needs |
| `native/src/media/encoder_probe.rs` | Boot-time probe: for each (codec, vendor) tuple try `avcodec_find_encoder_by_name` + 64×64 throwaway open. Returns `CodecCapability[]` matching the existing shape. | new (~150 LOC) |
| `native/src/media/bitrate_preset.rs` | Per-codec/vendor option strings (`preset=llhp` NVENC, `usage=lowlatency quality=speed` AMF, `preset=veryfast` QSV). Pure data. | mined from `tauri-client/.../bitrate_preset.rs` |

**Modified files**:

| File | Change |
|------|--------|
| `native/Cargo.toml` | Add `ffmpeg-next = "8"` under `[target.'cfg(target_os = "windows")'.dependencies]`. Add `Graphics_Capture`, `Win32_Graphics_Direct3D11`, `Win32_System_WinRT_Graphics_Capture`, `Win32_Graphics_Dxgi` features to the existing `windows` crate dep. |
| `native/src/lib.rs` | Register new `probe_native_encoders` and `force_keyframe` commands. |
| `native/src/state.rs` | `AppState.video_engine` gains Windows-side capture+encoder fields (under `cfg(target_os = "windows")`). |
| `native/src/commands/streaming.rs` | `start_screen_share` on Windows boots the native pipeline; `stop_screen_share` joins threads; `send_video_frame` becomes a no-op on Windows; new `force_keyframe` command sets the shared AtomicBool. |
| `native/src/commands/mod.rs` | Register the new commands. |
| `native/src/media/mod.rs` | `VideoEngine` re-export gains Windows fields. |
| `src/features/voice/streaming/StreamCapture.ts` | Windows branch (`if (window.decibell.platform === "win32")`): skip `getDisplayMedia` + `VideoEncoder.configure` entirely. Pass `sourceId` to `start_screen_share` and return immediately. `subscribeLocalFrames` remains for Linux/macOS only. |
| `src/utils/encoderProbe.ts` | On Windows: call `invoke('probe_native_encoders')`, use the returned caps directly, skip the WebCodecs probe. Cache key `decibell.native_encoder_caps.v1`. |
| `electron-builder.yml` | `win.extraResources` glob for the FFmpeg DLLs (avcodec, avutil, avformat, swscale, swresample). |
| `.github/workflows/electron-release.yml` | Windows job: `vcpkg install ffmpeg[nvcodec,amf,qsv]:x64-windows`; set `VCPKG_ROOT` env var; then existing native build. |
| `electron-client/PACKAGING.md` | Document the local `VCPKG_ROOT` setup. |
| `THIRD_PARTY_LICENSES.md` (new, Windows section) | FFmpeg LGPL license text + redistribution acknowledgement. |

Roughly 1100 LOC new/revived, ~250 LOC modified.

---

## 5. Capture: WGC details

**Source id parsing** (`window.decibell.capture.listSources` already returns these from Chromium's `desktopCapturer`):

- `screen:<N>:0` — index `N` into the array Chromium internally enumerates. We resolve it by calling `EnumDisplayMonitors` and mapping by index, then `IGraphicsCaptureItem::CreateFromMonitor(HMONITOR)`.
- `window:<HWND>:0` — decimal HWND as integer. We cast and call `CreateFromWindow(HWND)`.
- Anything else: return error, picker re-opens.

**WGC initialization**:
```rust
let item = match parse_source_id(&id)? {
    Source::Monitor(hmon)  => GraphicsCaptureItem::create_from_monitor(hmon)?,
    Source::Window(hwnd)   => GraphicsCaptureItem::create_from_window(hwnd)?,
};
let pool = Direct3D11CaptureFramePool::create_free_threaded(
    &d3d11_device_as_idirect3ddevice,
    DirectXPixelFormat::B8G8R8A8UIntNormalized,
    /* numberOfBuffers */ 2,
    item.size()?,
)?;
let session = pool.create_capture_session(&item)?;
session.set_is_border_required(false)?;  // hide the yellow border
session.set_is_cursor_capture_enabled(true)?;
session.start_capture()?;
```

**FrameArrived** handler runs on a WinRT-managed pool thread. We pull the `ID3D11Texture2D` out of the frame, send it to the encoder thread via `mpsc::Sender::try_send`. On `try_send` full (encoder is behind), we drop the oldest by `recv`-ing once and re-sending — keeps latency from runaway. A `frames_dropped` counter increments and feeds the per-second telemetry log.

**Resize handling**: WGC's `FrameContentSize` can change mid-session (target window resized, monitor mode changed). We compare against the encoder's configured width/height; if differs we call `pool.recreate(...)` and reconfigure the encoder. Mid-stream resolution change is uncommon but free to support — costs maybe a single keyframe.

**Teardown**: stop flag flipped → capture thread stops the session, drains the pool, drops the sender. Encoder thread's `recv()` returns `None`, encoder drains, threads join. `start_screen_share` waits for join before returning on a subsequent Go Live.

---

## 6. Encoder: FFmpeg details

**Vendor priority**: NVENC → AMF → QSV. Auto-detected by reading the existing `GPU device` info (vendor id) and ordering accordingly:
- NVIDIA vendor id `0x10DE` → try NVENC first
- AMD vendor id `0x1002` → try AMF first
- Intel vendor id `0x8086` → try QSV first
- Unknown / multi-GPU → try in NVENC/AMF/QSV order

For each codec the user requests, we walk the priority list and pick the first one whose `avcodec_open2` returns 0 with the current resolution/fps/bitrate/D3D11 device. The picked encoder is logged.

**D3D11 binding**:
```rust
let hwdevice_ctx = av_hwdevice_ctx_alloc(AV_HWDEVICE_TYPE_D3D11VA);
let dev = (*hwdevice_ctx.data).as_mut::<AVD3D11VADeviceContext>();
dev.device = d3d11_device.as_raw();
dev.device_context = d3d11_context.as_raw();
av_hwdevice_ctx_init(hwdevice_ctx);

let hwframes_ctx = av_hwframe_ctx_alloc(hwdevice_ctx);
let frames = (*hwframes_ctx.data).as_mut::<AVHWFramesContext>();
frames.format = AV_PIX_FMT_D3D11;
frames.sw_format = AV_PIX_FMT_NV12;
frames.width = width;
frames.height = height;
frames.initial_pool_size = 4;
av_hwframe_ctx_init(hwframes_ctx);

codec_ctx.hw_frames_ctx = av_buffer_ref(hwframes_ctx);
```

Once bound, `avcodec_send_frame` with an `AVFrame(AV_PIX_FMT_D3D11)` whose `data[0]/data[1]` point at our NV12 texture is zero-copy on all three vendors.

**Per-vendor options** (in `bitrate_preset.rs`, applied at open):

| Vendor | Codec | Preset string |
|--------|-------|---------------|
| NVENC | h264/hevc | `preset=p4` (or `llhp` on older builds), `tune=ull`, `rc=cbr`, `b_ref_mode=disabled`, `zerolatency=1` |
| NVENC | av1 | `preset=p4`, `tune=ull`, `rc=cbr`, `tile_columns=2`, `tile_rows=1` |
| AMF | h264/hevc | `usage=lowlatency`, `quality=speed`, `rc=cbr`, `enforce_hrd=true` |
| AMF | av1 | `usage=lowlatency`, `quality=speed`, `rc=cbr` |
| QSV | h264/hevc | `preset=veryfast`, `look_ahead=0`, `rdo=0`, `low_power=1` |
| QSV | av1 | `preset=veryfast`, `look_ahead=0` |

`AVCodecContext.bit_rate` and `rc_max_rate` are set from `videoBitrateKbps * 1000`. GOP size is `fps * 4` (one keyframe every 4 s) plus on-demand keyframes via `force_keyframe`.

**Keyframe path**: `Arc<AtomicBool> force_kf`. Encoder thread checks before each `send_frame`; if set, sets `AVFrame.pict_type = AV_PICTURE_TYPE_I` and clears the flag. Reset is idempotent. Set by:
- The new `force_keyframe` IPC command — wired from the existing `keyframe_requested` path in `media/peer.rs` (server-relayed PLI).
- The Plan C codec selector when a low-cap watcher joins.

**Bitrate adaptation**: encoder thread tracks NACK count over a 5 s sliding window. Once per second, computes ratio = nacks / packets_sent. If ratio > 0.05 → decrease by 25 % toward `min_bitrate=300_000`. If ratio < 0.01 → increase by 10 % toward `configured_bitrate`. Applied via `codec_ctx.bit_rate = new_rate; codec_ctx.rc_max_rate = new_rate * 3 / 2`. NVENC and AMF accept this without re-init; QSV requires a `avcodec_close` + `avcodec_open2` cycle (handled internally, costs one keyframe).

**Encode loop**:
```rust
loop {
    let texture = match frame_rx.recv() { Some(t) => t, None => break };
    let nv12 = video_processor.convert(texture)?;
    let mut avframe = wrap_d3d11_as_avframe(nv12, width, height);
    if force_kf.swap(false, Ordering::Relaxed) {
        avframe.pict_type = AV_PICTURE_TYPE_I;
    }
    avcodec_send_frame(codec_ctx, &avframe);
    loop {
        match avcodec_receive_packet(codec_ctx, &mut packet) {
            0 => {
                video_sender.send(packet.data, packet.is_keyframe(), description);
                events::send_stream_frame(local_username, &packet);  // self-preview
                av_packet_unref(&mut packet);
            }
            AVERROR(EAGAIN) => break,
            other => return Err(other),
        }
    }
    maybe_apply_bitrate_adjustment();
}
// drain
avcodec_send_frame(codec_ctx, ptr::null());
drain_remaining_packets();
```

---

## 7. IPC contract

| Command | Direction | Behavior (Windows) |
|---------|-----------|--------------------|
| `start_screen_share` | JS → native | Args: `{ sourceId, serverId, channelId, fps, width, height, videoBitrateKbps, shareAudio, audioBitrateKbps, initialCodec, enforcedCodec }`. Opens WGC on the parsed source id, configures the encoder, spawns the two threads, returns `{ width, height }` echoing the actual capture dims. Errors → renderer toast + picker reopens. |
| `stop_screen_share` | JS → native | Sets stop flag, joins capture + encoder threads (with a 2 s timeout — log and orphan if it hangs), releases D3D11 resources. |
| `send_video_frame` | JS → native | **No-op on Windows.** Kept for source compatibility so the renderer code can compile without `cfg` gates. Renderer simply doesn't call it on Windows. |
| `force_keyframe` | JS → native | **New.** Sets the shared `AtomicBool`. Wired from the existing `keyframe_requested` event path. |
| `probe_native_encoders` | JS → native | **New, Windows only.** Returns `CodecCapability[]`. The renderer reads this at boot (before the codec dropdown is rendered) and stashes it in localStorage with key `decibell.native_encoder_caps.v1`. |
| `set_encoder_caps` | JS → native | Unchanged on Linux/macOS. **Ignored on Windows** — native is the source of truth there. |

Renderer changes are limited to `StreamCapture.ts` (Windows branch in `start()` / `stop()`) and `encoderProbe.ts` (Windows path calls native). `CaptureSourcePicker.tsx` and `StreamVideoPlayer.tsx` are unchanged.

---

## 8. Build, distribution, licensing

**vcpkg ports**: `ffmpeg[nvcodec,amf,qsv]:x64-windows`. Explicitly omitting `x264`, `x265`, `libaom` — GPL contamination via dynamic linking. No software fallback in native as a consequence.

**Cargo.toml** (additions only):
```toml
[target.'cfg(target_os = "windows")'.dependencies]
ffmpeg-next = "8"
windows = { version = "0.61", features = [
    # existing features plus:
    "Graphics_Capture",
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Dxgi",
    "Win32_System_WinRT_Graphics_Capture",
] }
```

**Local dev**: developer installs vcpkg + the FFmpeg port once, sets `VCPKG_ROOT`. Post-build step in `native/package.json` copies the DLLs from `%VCPKG_ROOT%\installed\x64-windows\bin\*.dll` next to the `.node` file. README + `PACKAGING.md` document this.

**CI** (`.github/workflows/electron-release.yml`, Windows job only):
```yaml
- name: Install FFmpeg via vcpkg
  run: vcpkg install ffmpeg[nvcodec,amf,qsv]:x64-windows
- name: Set VCPKG_ROOT
  shell: pwsh
  run: echo "VCPKG_ROOT=$env:VCPKG_INSTALLATION_ROOT" >> $env:GITHUB_ENV
- name: Build native addon
  env:
    CMAKE_POLICY_VERSION_MINIMUM: "3.5"  # existing
  run: npm run build:native
```

**electron-builder.yml**:
```yaml
win:
  extraResources:
    - from: "electron-client/native/avcodec-*.dll"
      to: "."
    - from: "electron-client/native/avutil-*.dll"
      to: "."
    - from: "electron-client/native/avformat-*.dll"
      to: "."
    - from: "electron-client/native/swscale-*.dll"
      to: "."
    - from: "electron-client/native/swresample-*.dll"
      to: "."
```

Windows DLL search order finds the DLLs next to the `.node` file automatically. No PATH manipulation at runtime.

**Binary size delta**: ~+30 MB for FFmpeg DLLs. Installer grows from ~50 MB to ~80 MB on Windows. Acceptable for a streaming app.

**Licensing**: FFmpeg LGPL via dynamic linking. We add `THIRD_PARTY_LICENSES.md` (Windows section) carrying the LGPL text + the "request for sources" note pointing at https://ffmpeg.org. nvcodec/amf/qsv link against vendor SDKs (NVIDIA Video Codec SDK, AMD AMF, Intel oneVPL) which permit redistribution as part of products. No GPL components in the chain.

---

## 9. Failure handling

| Failure | Recovery |
|---------|----------|
| `start_screen_share` source id doesn't parse | Native error → renderer toast "Couldn't open the selected source" → picker reopens. |
| WGC fails to create from HMONITOR / HWND (window closed mid-flow) | Same as above. |
| All three encoder vendors fail to open | Native returns "No hardware encoder available — install your GPU's video drivers" → renderer toast → Go Live aborts. |
| `probe_native_encoders` returns empty list at boot | Codec dropdown shows only an informational entry: "No hardware encoder detected." Go Live button disabled. |
| Encoder fails mid-stream (driver crash, GPU reset) | Encoder thread logs the error, sends a `stream_failed` event to renderer, sets stop flag. Renderer tears down via existing `onCaptureEnded` callback and re-emits the picker. |
| Mid-stream resolution change (window resized) | Capture thread detects via `FrameContentSize`, recreates the pool, signals encoder thread which reconfigures the encoder. Costs one keyframe. |
| Thread join timeout on stop | Log + orphan + proceed. Next `start_screen_share` may overlap briefly; D3D11 resources are reference-counted so this is safe but logged. |

---

## 10. Testing strategy

**Manual matrix** (developer's RTX 4080 + ideally a second physical Windows box):
- Resolutions: 1080p30, 1080p60, 1440p60, 4K30, 4K60
- Codecs: H.264, H.265, AV1
- Source kinds: full screen (primary monitor), full screen (secondary monitor), single window (Chrome / Discord / a fullscreen game)
- Cycles: rapid Go Live → Stop → Go Live (10×) to exercise thread join + restart
- Mid-stream codec switch (Plan C path firing on a low-cap watcher joining) — exercises `force_keyframe`
- Self-preview: own tile renders frames via the unified stream_frame TSFN
- Cross-OS watch: Windows native sender, Linux WebCodecs watcher — wire format parity

**Telemetry**: encoder thread logs once per second to stderr:
```
[encoder] codec=h264_nvenc 1920x1080@60 8.2Mbps (target 10.0Mbps) frames_sent=60 frames_dropped=0 nack_ratio=0.02
```
Drop count + NACK ratio surface regressions at a glance.

**Unit tests** (in the new files where reasonable):
- Source id parsing (`screen:0:0`, `window:65998:0`, malformed inputs)
- Bitrate adaptation math (ratio thresholds, increase/decrease step sizes)
- Encoder probe handles missing FFmpeg DLLs gracefully (returns empty caps, not panic)

No backend telemetry — local logs only for this milestone.

---

## 11. Rollback

Every platform-conditional change is gated by `cfg(target_os = "windows")` or `window.decibell.platform === "win32"`. If the Windows path explodes in production:

- `git revert` the merge / squash commit. Linux + macOS unaffected.
- The picker UI and source-id IPC (already shipped) are unchanged by this design; the revert is purely the streaming path.

We deliberately don't add a `DECIBELL_FORCE_WEBCODECS_WINDOWS=1` env var as a runtime fallback. YAGNI: it adds branching to ship and test, and `git revert` covers the bad-release case adequately. If a specific user-facing bug surfaces after the release that can't be reproduced internally, that env var can be added in a hotfix.

---

## 12. Open questions deferred to implementation

- Exact source id format for multi-monitor configurations — verify Chromium's `desktopCapturer.getSources` numbering matches `EnumDisplayMonitors` order, or whether we need to match by display device path. (5-minute test once we're coding.)
- Whether NVENC `preset=p4` or `preset=llhp` is the correct option string for the current FFmpeg version — both work but the option key changed across versions. The probe path will dispatch.
- D3D11 device feature level: 11_0 vs 11_1. Current Tauri code used 11_0; verify it still satisfies NVENC encoder requirements.

None of these are design questions — they're tactical decisions made during implementation against the real `ffmpeg-next` 8 + `windows` 0.61 surfaces.

---

## 13. Scope summary

Approximately 1100 LOC new or revived (most mined from Tauri-era code), 250 LOC modified. Concentrated in `native/src/media/` (capture + encode), one renderer file (`StreamCapture.ts`), one encoder-probe file, CI config, and the licensing doc. All platform-gated. Linux and macOS behave identically to today.
