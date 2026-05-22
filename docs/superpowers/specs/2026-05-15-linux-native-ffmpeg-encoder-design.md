# Linux native FFmpeg stream encoder — design

Status: in progress (started 2026-05-15). Native side (L1–L7) complete +
builds + links FFmpeg 8 + napi bindings generated. Remaining: L8 renderer
wiring, L9 packaging, L10 NVIDIA smoke test.

**Dev env** (the box this is tested on): KDE Plasma / KWin + NVIDIA 595.71
open kernel module + Wayland + xdg-desktop-portal-kde. wlr-screencopy
path is inactive here (KWin has no zwlr_screencopy); capture goes through
the portal.

**Decision (2026-05-21):** re-enable true DMA-BUF zero-copy (disabled
upstream in Tauri due to the NVIDIA+mesa black-frame bug). Sequencing:
ship + verify the CPU-capture→NVENC-GPU-convert baseline FIRST (L8–L10),
then re-advertise DMA-BUF in `capture_pipewire`'s format pod and A/B it
against that baseline so a black-frame regression is immediately visible.

**RESULT (2026-05-22): zero-copy is environmentally blocked on the dev box
and the attempt is closed.** Added an env-gated probe (`DECIBELL_TRY_DMABUF=1`
offers DMA-BUF before SHM in the format-pod order; default stays SHM-first).
On KDE Plasma + KWin + NVIDIA-595 + Wayland + portal it negotiates
`dmabuf=true` but with `modifier=DRM_FORMAT_MOD_INVALID (0xff…ff)`, and the
buffers arrive `DataType::Unknown / has_data=false / stride=0` — i.e. an
unspecified tiled layout that PipeWire's mmap can't read AND that EGL→CUDA
can't import (EGL needs a real modifier; INVALID → black). This is the
well-known NVIDIA-proprietary + Wayland + portal screencast limitation, not
a code bug. Only an X11 session or a wlroots compositor +
xdg-desktop-portal-wlr (which allocate LINEAR / real-modifier DMA-BUF) can
supply a usable buffer. **The default SHM path (CPU capture → NVENC on-GPU
BGRA→NV12, no CPU sws_scale) is the correct, shipped solution** — full
hardware encoding; the only extra cost vs zero-copy is one frame memcpy +
GPU upload. The DMA-BUF→CUDA path + the `DECIBELL_TRY_DMABUF` flag remain as
a working escape hatch for wlroots/X11 users; dormant otherwise.
Sibling: `2026-05-12-windows-native-ffmpeg-encoder-design.md` (the Windows
counterpart this mirrors), `../plans/2026-04-02-dmabuf-zero-copy-capture.md`
(the original DMA-BUF design, written for the Tauri client).

## Why

On Linux, stream video is encoded in the renderer via Chromium WebCodecs
(`prefer-hardware`). That uses VAAPI fine on AMD/Intel, but on NVIDIA the
Electron-33 Chromium build does **not** reach NVENC — NVIDIA users fall
back to software encoding (slow, fps-capped). This brings the Windows
"native owns capture + encode" model to Linux so NVIDIA users get NVENC,
and AMD/Intel get a native VAAPI path too.

## Source of truth

The Tauri client (`tauri-client/src-tauri/src/media/`) shipped this exact
pipeline. We revive it into `electron-client/native/src/media/`:

| Tauri file | Lines | Revived as | Role |
|------------|-------|-----------|------|
| `encoder.rs` (Linux paths) | ~1978 | extend `encoder.rs` (cfg-gated) | `new_cuda`/`new_vaapi`/`libx264`, encode_* |
| `gpu_interop.rs` | ~1283 | `gpu_interop.rs` (new) | DMA-BUF → CUDA / VAAPI zero-copy |
| `capture_pipewire.rs` | ~1078 | `capture_pipewire.rs` (new) | PipeWire/XDG-portal capture |
| `capture_wlr_screencopy.rs` | — | `capture_wlr_screencopy.rs` (new) | wlroots fast path |
| `video_pipeline.rs` (run loop) | ~668 | new Linux pipeline fn | capture→encode→send wiring |
| `capture.rs` (frame types) | — | extend `capture.rs` (cfg-gated) | RawFrame / DmaBufFrame / PixelFormat |

## Architecture (mirrors Windows, swaps the platform layer)

```
XDG ScreenCast portal (or wlr-screencopy)         [capture_pipewire.rs]
   → PipeWire stream: DMA-BUF fd (preferred) or BGRA SHM frame
      ├─ DMA-BUF → gpu_interop:                    [gpu_interop.rs]
      │    NVIDIA: EGLImage → GL tex → cuGraphicsGLRegisterImage
      │            → cuMemcpy2D into tight CUdeviceptr (BGRA)
      │    AMD/Intel: AVDRMFrameDescriptor → av_hwframe_map → VAAPI surface
      └─ SHM BGRA → CPU path (sws_scale BGRA→NV12)
   → H264Encoder (FFmpeg):                         [encoder.rs]
      NVENC: pix_fmt=CUDA, sw_format=BGRA (GPU does BGRA→NV12), h264/hevc/av1_nvenc
      VAAPI: pix_fmt=VAAPI, h264/hevc/av1_vaapi
      sw:    libx264 (NV12)
   → EncodedFrame (AVCC/HVCC/OBU + decoder description on keyframes)
   → VideoSender (frame-sink slot) → UDP packetise                [video_pipeline.rs, shared]
   → self-preview: per-stream TSFN → renderer StreamVideoPlayer    [events.rs, shared]
   → thumbnail every 3s: downscale + JPEG → send_stream_thumbnail  [shared]
```

The send path (`VideoSender` / frame-sink slot / `UdpVideoPacket` / TSFN
self-preview / thumbnails) is **already shared and unchanged** — the
Windows pipeline already feeds it. We only add the Linux capture+encode
front half.

## Key integration decisions

- **Two backends**: NVENC (CUDA) + VAAPI. Software libx264 stays as the
  last-resort fallback. Probe order by GPU vendor, same as Windows
  `encoder_probe`.
- **Capture owns source selection**: the XDG portal shows the DE's own
  screen-share picker. On the native Linux path the in-app Chromium
  `desktopCapturer` picker is bypassed — `start_screen_share` triggers the
  portal instead of taking a Chromium `sourceId`. (Renderer L8.)
- **send_video_frame becomes a no-op on Linux when native is active**,
  exactly like Windows — the native pipeline owns the encode→send end to
  end. The renderer's WebCodecs path remains as the non-native fallback
  (AMD/Intel users who don't want native, or if portal/PipeWire is absent).
- **Degradation**: gpu_interop falls back to CPU readback (sws_scale) when
  EGL/GL/CUDA or VAAPI libs are missing at runtime, so a thin install
  still streams (software) instead of failing.
- **FFmpeg linkage (RESOLVED 2026-05-21 — L7b)**: must **statically** link
  FFmpeg, not dynamic. Electron bundles Chromium's `libffmpeg.so` which
  exports decode-only FFmpeg symbols (`avcodec_find_encoder_by_name`, …)
  into the global scope; a dynamically-linked addon binds its `avcodec_*`
  calls to *those* symbols, so the encoder probe finds nothing (verified:
  `ELECTRON_RUN_AS_NODE` probe returned `[]`, every encoder "not
  registered"). RTLD_DEEPBIND fixes symbol resolution but breaks CUDA
  (`cuInit → OUT_OF_MEMORY`), so it's a dead end. Fix: build a minimal
  static LGPL FFmpeg (NVENC+VAAPI, no libx264 → no SW fallback) via
  `native/scripts/build-ffmpeg-linux.sh` → `native/vendor/ffmpeg`
  (gitignored), enable the `static` feature on ffmpeg-next, and point
  pkg-config at it via `native/.cargo/config.toml` (`FFMPEG_DIR` must stay
  unset so the pkg-config branch resolves transitive deps via
  `Libs.private`). Static symbols are internal to the `.node` → nothing
  for Electron's libffmpeg to shadow, and NVENC's `libnvidia-encode` loads
  normally. This mirrors Windows (vcpkg DLLs, immune by name-based import).
  Bonus: no runtime system-ffmpeg dependency, and CI no longer cares about
  the distro FFmpeg version. **Verified**: plain build → probe under
  Electron returns h264/hevc/av1_nvenc.

## Phases

L1 build deps · L2 frame types · L3 encoder backends · L4 gpu_interop ·
L5 capture · L6 pipeline · L7 commands · L8 renderer · L9 packaging ·
L10 NVIDIA smoke test. (Tracked in the task list.)

## Validation

NVENC and VAAPI can't run in CI (no GPU). CI only proves it **compiles**
on Ubuntu (libav*-dev added to the runner). Runtime validation is local on
the developer's NVIDIA CachyOS box (L10): confirm the native path engages
(log line shows `h264_nvenc`, not WebCodecs), self-preview renders, a
remote watcher decodes, thumbnails arrive.
