# Encoders on every vendor, software as the last resort

2026-11-26. Owner's rule (CLAUDE.md, "Video codecs: hardware on every vendor, software only as the
last resort"): screen shares are encoded on the GPU on NVIDIA, AMD and Intel, on Linux and Windows,
integrated GPUs included; only a machine with no usable hardware encoder falls back to software;
a vendor path that silently fails is a missing feature, so probe for real, fall through in order,
and say which path is in use. The software encoder is SVT-AV1 (royalty-free, real-time); no software
H.264 (OpenH264 brings patent-licence strings).

The encoder lives in the shared core (`crates/decibell-core/src/media/`) and serves two clients:
the Electron addon, which still has Chromium's WebCodecs encoder to fall back to, and the GPUI
client, which has nothing else.

**What has run on hardware: NVENC (H.264, HEVC, AV1) and software AV1, on Linux, on an RTX 4080 /
i7-12700K. VAAPI, Quick Sync, AMF under the new probe, and everything `cfg(windows)` have only been
compiled, unit-tested and read.** §8 is the list and the one command per backend.

## 1. What was there

- Linux: `H264Encoder::new(codec)` tried `h264_nvenc`, `h264_vaapi`, … by opening each with a bare
  NV12 context. VAAPI cannot open that way (it needs a device and a frames context), so it never
  probed: AMD and Intel had no native encoder. A VAAPI branch existed behind a DMA-BUF capture path
  that the capture never fed (`gpu_interop.rs`). The software entry was `libx264`, which the LGPL
  FFmpeg build does not contain.
- Windows: the probe opened `*_nvenc` / `*_amf` at 1280×720 without encoding. Quick Sync was left out
  because `Encoder::open` could not drive it.
- A machine where nothing probed used the renderer's WebCodecs encoder (Electron only).

## 2. Backends and order (`encoder_backend.rs`)

`Backend`: `Nvenc`, `Amf` (Windows), `Qsv` (Windows), `Vaapi` (Linux), `Software`. Each names its
FFmpeg encoder per codec (`h264_vaapi`, …; software is `libsvtav1` and AV1 only).

The order a machine tries them in is built from its GPUs (`Adapter`: PCI vendor and device ids,
driver build, primary flag, and on Linux the DRM render node):

- **Linux**: NVENC on an NVIDIA adapter → VAAPI on each AMD / Intel render node, the boot VGA device
  (the compositor's) first → software.
- **Windows**: NVENC and AMF on their adapters, AMF first when the primary adapter is AMD → Quick Sync
  on an Intel adapter → software.

Decisions in that order:

- **An Intel-primary laptop with an NVIDIA or AMD GPU keeps NVENC / AMF first on Windows.** The brief
  was "prefer the encoder on the GPU that holds the captured frames". On Windows the frames are
  wherever the capture device is created (WGC delivers a window or monitor to any adapter's device,
  and `GpuDevice::create_for_vendor` already creates it on the encoder's adapter), and these laptops
  encode with the dedicated GPU today. Putting the untested Quick Sync path in front of a working
  NVENC would change every such machine. Flip the two entries in `backend_order` if Quick Sync
  proves better there.
- **A backend is only tried on adapters of its vendors.** A machine known to have no NVIDIA GPU does
  not load NVENC to watch it fail. If the adapter list is empty (enumeration failed), every backend
  of the OS is tried without one.
- **Software is offered only when no hardware backend encodes anything.** A GPU that encodes H.264
  but not AV1 does not get AV1 from the CPU: the owner's rule is "only a machine with no usable
  hardware encoder falls back to software".
- `DECIBELL_ENCODER=nvenc|amf|qsv|vaapi|software` (every build) leaves only that backend: it is
  probed, used, and nothing falls through to another. An unknown value is ignored with a warning.

## 3. The probe (`encoder_probe.rs`)

A test encode is the stream's own path at a small size: the encoder is opened as a stream opens it
(same options, same device, same input conversion) at 640×360, 30 fps, 1 Mbps, three frames of a
generated picture go through `encode_bgra_frame` (Linux) or `send_bgra` on a D3D11 texture
(Windows), and it counts only if a packet comes out and the first is a keyframe.

Per codec the first hardware candidate that works wins and later ones are not opened; software is
tested only when no hardware cap came out. The result (`ProbeReport`) is the caps, each naming its
backend, vendor, encoder and label ("VAAPI (AMD)"), and every candidate that was skipped with the
reason; both are logged (`[encoder-probe] …`).

**When**: never at startup. `probe()` runs when the client asks for the list
(`api::streaming::probe_native_encoders`, as before), `cached()` at the first stream start that
finds no result and from `api::streaming::cached_native_encoders` (new; for a client without its
own cache). Both run on the blocking pool. Measured here: 0.8 s for the three NVENC test encodes in
a debug build on a busy machine, most of it the first NVENC session.

**Cache**: process-wide, and `<user data>/encoder_probe.json`, keyed by a fingerprint of every
adapter (ids, primary, render node, driver build), `DECIBELL_ENCODER`, the core's and the app's
versions and which encoders the linked FFmpeg has. A result with no encoder at all is not trusted
from the cache (the next question probes again), as the Windows probe already did. Driver build: the NVIDIA kernel module version; for AMD / Intel on
Linux the kernel release plus size and date of the VA-API driver file; on Windows the user-mode
driver version from DXGI (`CheckInterfaceSupport`). A stream start corrects the entry: a candidate
that fails is recorded (`note_failed`), the one that carries the stream becomes the codec's cap
(`note_working`).

**API** (additive): `NativeEncoderCap` gains `backend`, `label` and `max_pixel_rate` (software: the
pixels a second the CPU is allowed; 0 for hardware); `StartScreenShareResult` gains `codec` and
`encoder`. The Electron renderer reads neither yet.

## 4. Stream start and fallthrough

`start_screen_share` asks the probe for a plan (`encoder_backend::start_plan`): every hardware
candidate in order that has an encoder for the requested codec and was not seen to fail, then
software when it is in the order and the stream may be AV1 (the requested codec is AV1, or
`enforced_codec` is 0). "Auto" (codec 0) is the first codec with a cap, as before.

- **Linux** (`encoder_thread_linux.rs`): the first choice that opens runs the stream. Until it has
  produced a packet it is on trial: three failed frames in a row, or two seconds of frames with no
  packet, and the next choice is opened in place on the same capture. Only when the list is used up
  does the stream end with `native_stream_failed`. After the first packet the old rule applies (60
  consecutive failures end the stream).
- **Windows**: open failures fall through in `VideoEngine::start_windows`, which restarts the
  capture on the next choice's adapter (the capture device belongs to the encoder's GPU). A capture
  that cannot start ends the attempt at once: no encoder would change that. Inside the thread, a
  hardware encoder that fails before its first packet is replaced by the software encoder on the
  same device, when the plan allows it.
- One pass over the list. No retries, no timers.
- The stream is announced with the codec it started on. Electron always pins the codec
  (`enforcedCodec`), so there it never changes; a client that does not pin it may be told AV1 after
  asking for H.264 (`StartScreenShareResult.codec`).
- Software is skipped, without being marked failed, for a stream above its limits (§6).

User-visible events are unchanged: a failed start is the command's error (Electron then falls back
to WebCodecs), `native_stream_failed` is the last backend failing mid-stream, `stream_capture_ended`
the source going away.

## 5. Per-backend settings

Every option name was checked against FFmpeg 8.0.1 (vcpkg's, the addon's) and 8.1.3 (the GPUI
client's). `bitrate_preset.rs` holds the tables and tests that the names stay spelled right.

### NVENC (both OSes): unchanged

Linux: BGRA in, `preset=p5 rc=cbr tune=ull` (`tune=ll slices=1` for HEVC), `forced-idr=1`, VBV of
four frames / 400 ms. Windows: D3D11 NV12, `preset=p4 tune=ull rc=cbr`. The diff to the NVENC code
is the removal of dead branches around it.

Found while measuring, not changed: `hevc_nvenc` with `tune=ll` produced 56% of its target bitrate
on the test screen (H.264 100%, AV1 101%).

### VAAPI (Linux: AMD radeonsi, Intel iHD / i965): `encoder_vaapi.rs`

Input, two tiers chosen at open and logged:

1. **GPU convert.** The BGRA frame is copied once into a BGRX VAAPI surface
   (`av_hwframe_transfer_data`); FFmpeg's `scale_vaapi` (the driver's video processor) converts to
   NV12 and scales to the encode size: `buffer → scale_vaapi=format=nv12:out_color_matrix=bt709:
   out_range=tv → buffersink`, what `ffmpeg -vf hwupload,scale_vaapi` does. BGRX (`VA_FOURCC_BGRX`)
   before BGRA: a monitor capture's fourth byte is padding, and a processor that read it as alpha
   would output black. Source: `libavutil/hwcontext_vaapi.c` (format map, upload),
   `libavfilter/vaapi_vpp.c`, `vf_scale_vaapi.c`.
2. **CPU convert**, when the FFmpeg build has no `scale_vaapi`, the driver takes no RGB surfaces, or
   its VPP refuses the conversion: swscale to NV12 (BT.709, threaded) and an NV12 upload.
   `DECIBELL_VAAPI_CONVERT=cpu` forces it.

No DMA-BUF import: the capture delivers SHM frames (the DMA-BUF path gave black frames with
mesa-allocated buffers on NVIDIA and was never fed).

Encoder (`h264_vaapi` / `hevc_vaapi` / `av1_vaapi`), source `libavcodec/vaapi_encode.c`,
`hw_base_encode.c`:

| Setting | Why |
|---|---|
| `rc_mode` CBR, then VBR (peak = target), then FFmpeg's choice | CBR is what the presets mean. radeonsi lists CQP / CBR / VBR, iHD CQP / CBR / VBR / ICQ / QVBR. A driver without CBR fails the open ("Driver does not support CBR RC mode"; Intel low-power CBR without HuC firmware) and gets the next. The mode that opened is logged. |
| `rc_buffer_size` = four frames, at most 400 ms | the HRD buffer, as NVENC's VBV |
| `max_b_frames = 0` | no reordering |
| `async_depth=1` | the default (2) holds a frame back until the next arrives (`hw_base_encode.c` receive_packet) |
| `idr_interval=0` | every GOP's I frame is an IDR |
| `low_power` left at 0 | FFmpeg then accepts `VAEntrypointEncSlice` or `EncSliceLP`, whichever the driver has (radeonsi: EncSlice only; newer Intel, Arc / Meteor Lake: EncSliceLP only). `low_power=1` insists on EncSliceLP and fails on AMD. |
| forced keyframe | `pict_type = I` on the frame is a forced IDR (`hw_base_encode.c`); no option |
| parameter sets | with packed-header support (iHD; recent radeonsi) FFmpeg writes SPS / PPS / VPS / the AV1 sequence header with every IDR; a driver without it writes its own with every IDR. The encoder still caches the last H.264 parameter sets and the last hvcC for a keyframe that arrives without. |
| AV1 description | none: the sequence header is in band (NVENC AV1 on Linux keeps sending its extradata, as before) |
| quality level | the driver's default (`compression_level` means a different scale per driver); also for 5 fps `detail` streams |
| live bitrate change | not possible (`vaapi_encode` reads the rate at init), as with NVENC on Linux |

Render node: the adapter's (`/dev/dri/renderD*` from `/sys/class/drm`), `DECIBELL_VAAPI_DEVICE`
overrides.

What is said here about the drivers comes from FFmpeg's source and from how these drivers are known
to behave, not from running them. Caveats to expect: older Mesa wrote its own H.264 SPS instead of
FFmpeg's (a 1088-line picture for 1080p where it ignored the cropping); pre-Broadwell Intel has no
`iHD` encoder (`LIBVA_DRIVER_NAME=i965`).

### Quick Sync (Windows): `encoder.rs`

FFmpeg's own D3D11 → QSV route, the one behind
`ffmpeg -f lavfi -i ddagrab -vf hwmap=derive_device=qsv,format=qsv -c:v h264_qsv`
(`libavfilter/vf_hwmap.c`, `libavutil/hwcontext_qsv.c`):

1. a QSV device derived from the capture's D3D11 device (`av_hwdevice_ctx_create_derived`);
2. a QSV frames context derived from the D3D11 NV12 pool (`av_hwframe_ctx_create_derived`), which
   the encoder is opened with (`pix_fmt = AV_PIX_FMT_QSV`);
3. per frame, the pool texture the video processor filled is mapped to a QSV frame
   (`av_hwframe_map`): no copy, no readback.

The D3D11 pool is **dynamic** (`initial_pool_size = 0`) for Quick Sync. With a fixed pool of
render-target textures FFmpeg 8.0 creates the textures lazily but derives the QSV surface table from
the still-unset texture list at once (`d3d11va_frames_init`, `qsv_fixed_frames_derive_to`), so no
frame would ever map; this is the likely reason the Tauri-era attempt (kept as dead code in
`encoder_linux.rs` until now) never worked. A dynamic pool takes `qsv_dynamic_pool_map_to`, which
reads the texture from the frame. Bind flags are still tried in the NVENC ladder, by allocating
one frame.

Options (`libavcodec/qsvenc.h`): `preset=veryfast` (`slower` for `detail`), `async_depth=1` (default
4 buffers four frames), `forced_idr=1`, `scenario=displayremoting`, `look_ahead=0` (H.264 only);
`low_power` left at its default (-1): Arc / Xe only have the low-power encoder and pick it, older
parts fail when it is forced. CBR from `bit_rate == rc_max_rate`. The Windows keyframe safety net
(`for_each_packet` splices extradata parameter sets into a keyframe without) covers H.264 / HEVC.

### AMF (Windows): unchanged options

`usage=lowlatency quality=speed rc=cbr forced_idr=1 …` as before. New for AMF: it is now probed by
encoding (three frames and a drain), not only opened.

### Software (both OSes): SVT-AV1, `encoder_software.rs`

`libsvtav1` with `pred-struct=1:scm=1:lookahead=0:enable-tf=0`, CBR (`bit_rate == rc_max_rate`), GOP
from the stream (2 s on Linux, 4 s on Windows, a minute for `detail`), buffer of four frames but at
least 100 ms, preset from the CPU (§6). Source: `libavcodec/libsvtav1.c`, SVT-AV1 4.2.0
`Docs/Parameters.md`, `Source/Lib/Globals/enc_handle.c`.

- Low delay makes the packet call block until the frame just sent is encoded
  (`svt_av1_enc_get_packet`): one packet back per frame within the call, like a hardware encoder.
- Not `rtc=1` (§6).
- A requested keyframe is `pict_type = I`; SVT-AV1 4.x honours it in low delay without a flag
  (only 4.2.0 was run; FFmpeg's `libsvtav1.c` notes that earlier versions took no keyframe requests,
  so the build is to use 4.0+, and the harness's forced-keyframe check is the guard).
- No description: every key frame starts with a sequence header OBU.
- Linux input: swscale BGRA → I420 with the BT.709 matrix and studio range (its default is BT.601;
  a test decodes pure red and blue to check), converting and scaling in one pass on 1 to 4 threads.
- Windows input (`nv12_readback.rs`): the D3D11 video processor converts and scales to an NV12
  texture as for the hardware encoders; that texture is copied to the next of three staging textures
  and `Flush`ed; staging textures are mapped with `D3D11_MAP_FLAG_DO_NOT_WAIT`, oldest first, on
  later iterations, and deinterleaved into the encoder's frame. Nothing waits on the GPU; a frame
  is dropped when all three are in flight. The encoder thread looks again every 4 ms while a
  readback is pending, so software on Windows runs about one frame behind.

FFmpeg builds: `crates/decibell-core/tools/ffmpeg-encoders.md`. The Electron addon's FFmpeg does not
get SVT-AV1 (7 MB for a path Electron covers with WebCodecs); the GPUI client's must.

## 6. Software measurements and limits

Machine: i7-12700K (8 P + 4 E cores, 20 threads), Linux, FFmpeg 8.1.3 + SVT-AV1 4.2.0 (static, nasm).
`encoder_harness::software_measurements`, release build: the generated screen at each size, fed in
real time through the stream's path (swscale BGRA → I420, then SVT-AV1), 5 s a row; the page of text
scrolls at 180 px/s (dense, hard-edged detail moving every frame, the costly case for a screen
encoder). Bitrates are the presets' at each size. "call ms" is how long one `encode_bgra_frame`
holds the encoder thread, conversion included; the frame's packet is back when it returns. CPU is
the whole process's minus generating the frames. Quality is PSNR-Y / SSIM of every 15th decoded
frame (dav1d) against the source converted to BT.709 luma. **The machine was not idle** (other
builds were running: load average 2.7 before, 5.2 after), so the times are upper bounds: the
1080p30 preset 8 row, for one, measured 16 ms a call and 43 ms of CPU in another run against
28 ms and 67 ms here. Quality and bitrate are deterministic.

| Size, fps, target | Preset | call ms mean / p95 | CPU ms a frame | CPU, % of one core | kbps out | PSNR-Y dB | SSIM |
|---|---|---|---|---|---|---|---|
| 720p30, 2500 | 8 | 11.0 / 20.5 | 25.8 | 78 | 2080 | 56.1 | 1.000 |
| | 10 | 6.3 / 12.7 | 13.7 | 41 | 2442 | 37.4 | 0.990 |
| | 12 | 10.5 / 28.9 | 14.9 | 45 | 2556 | 36.5 | 0.986 |
| 1080p30, 4000 | 8 | 28.1 / 56.2 | 66.7 | 201 | 3278 | 56.8 | 1.000 |
| | 10 | 10.5 / 19.2 | 24.6 | 74 | 4013 | 37.2 | 0.991 |
| | 12 | 7.3 / 12.8 | 18.2 | 55 | 4156 | 36.4 | 0.989 |
| 1080p60, 6000 | 8 | 15.7 / 29.3 | 43.8 | 254 | 5333 | 56.9 | 1.000 |
| | 10 | 7.4 / 12.8 | 18.7 | 113 | 5891 | 37.5 | 0.992 |
| | 12 | 7.5 / 13.9 | 17.3 | 104 | 5999 | 36.2 | 0.989 |
| 1440p30, 6000 | 8 | 26.2 / 50.3 | 74.1 | 223 | 4649 | 58.7 | 1.000 |
| | 10 | 14.4 / 24.2 | 39.0 | 117 | 6260 | 37.1 | 0.991 |
| | 12 | 12.1 / 19.7 | 31.2 | 94 | 6286 | 36.0 | 0.988 |
| 1440p60, 9000 | 8 | 23.5 / 41.5 | 71.0 | 281 | 7808 | 57.8 | 1.000 |
| | 10 | 11.9 / 18.3 | 32.5 | 195 | 8985 | 38.3 | 0.992 |
| | 12 | 11.0 / 16.7 | 28.3 | 170 | 9327 | 37.1 | 0.990 |

A still page (only the frame counter and a small window move) is not cheaper in proportion:
1080p60 at preset 10 took 15.7 ms of CPU a frame (94% of a core) against 18.7 scrolling, and the
encoder spent its whole bitrate refining the picture (46.7 dB). Software's cost is roughly its
pixel rate, whatever is on screen.

What the measurements decided:

- **Plain low delay, not `rtc=1`.** At 1080p30, 4 Mbps, preset 10: `rtc=1` gave PSNR-Y 27.8 dB,
  SSIM 0.921, 34 ms of CPU a frame and 3323 kbps; `pred-struct=1` alone gave 37.1 dB, 0.991, 20 to
  25 ms and 3863 to 4013 kbps. At preset 8, 48.7 dB against 56.8. SVT-AV1's real-time mode is tuned
  for camera video and drops what makes text cheap.
- **Preset 8 where the CPU has room, 10 otherwise, 12 on small CPUs** (`software_preset`, by the
  pixel rate each logical CPU carries). Preset 8 keeps the screen-content tools (palette, intra
  block copy): the text comes back within rounding (56 dB) at four fifths of the bitrate. It costs
  2 to 3 times the CPU of 10, and at 1080p30 its p95 ran past the 33 ms frame interval here, so it
  is used up to 720p30 on this class of machine (1.5 M pixels a second per logical CPU) and for
  5 fps `detail` streams. 9 costs twice what 10 does for a few dB and 11 measured the same as 12.
- **Threaded conversion.** swscale's BGRA → I420 took 6.0 ms a 1080p frame and 10.1 ms a 1440p one
  on one thread, as much as SVT-AV1's fast presets take to encode it. On 4 slice threads
  (`YuvScaler`, `sws_scale_frame`): 3.4 ms and 4.9 ms (median, the row copy included), and 5.0 ms
  for a 1440p capture scaled to 1080p in the same pass.
- **Limits** (`software_limits`): a pixel rate (width × height × fps) of 1080p30 for every 6 logical
  CPUs, at least 720p30, at most 1440p60, never above 1440p or 60 fps. On the measured machine
  1080p60 costs a little over one core and stays inside its frame interval (7.4 / 12.8 ms of
  16.7); 1440p60 does not reliably (p95 18.3 ms at preset 10, 16.7 at 12), and 20 threads' budget
  ends just under it. For a laptop the rule assumes cores half as fast: 1080p30 on 8 threads is
  then about 1.5 of its cores, a third of the machine. 4K is never offered in software.

| Logical CPUs | Software accepts |
|---|---|
| up to 5 | 720p30 (720p up to 45 fps at 4), 1080p up to 20 fps |
| 6 to 11 | 1080p30, 720p60 |
| 12 to 23 | 1440p30, 1080p60 (1440p up to 56 fps at 20) |
| 24 and up | 1440p60 |

The cap carries the limits (`max_width`, `max_height`, `max_fps`, `max_pixel_rate`) so a Go Live UI
can grey out what software cannot do; a start above them skips the software encoder with the reason.
The renderer's WebCodecs fallback advertises 1080p60 for software H.264 whatever the CPU; this is
the same idea with the CPU counted.

**Legibility**, as judged: besides PSNR / SSIM, decoded 1080p30 frames at presets 10 and 12
(4 Mbps, mid-scroll) were written out (`DECIBELL_HARNESS_DUMP`) and looked at next to the source.
Body text (10×14 px glyphs) and the small print (5×7 px) are sharp and fully readable at both; the
visible defect is a faint trail of the previous text behind the edge of the moving window. (The
first configuration tried, `rtc=1` at preset 10 at 21 dB, was readable too, with ringing around
every glyph.)

Keyframes: SVT-AV1's largest packet was 3.6× the average at 720p30 / 3 Mbps (NVENC: 4.0× for H.264,
10× for AV1), so the burst a keyframe puts on the wire is no worse than the hardware encoders'.

Not measured: a real 4-core laptop (the limits scale one desktop CPU), and Windows.

## 7. Tests

- `encoder_backend.rs`: the order per machine shape (table), forcing, probe selection over fake
  results, the start plan and its correction after failures, software limits and presets, the cache
  fingerprint.
- `encoder_probe.rs`: adapters from a fake `/sys/class/drm`, the real probe on the machine at hand.
- `encoder_harness.rs` (Linux): a generated screen (scrolling text, a moving window, a frame
  counter) through `encode_bgra_frame`; one packet a frame, self-contained keyframes, a forced
  keyframe within 2 frames, bitrate within tolerance over 5 s, increasing timestamps, the stream
  decoded by FFmpeg's software decoders with every frame back and PSNR / SSIM against the source.
  Runs for NVENC (H.264, HEVC, AV1) and software in every `cargo test --lib`, skipping where the
  backend is absent; `forced_backend_streams_are_sound` (ignored) is the per-backend command.
- `encoder.rs` tests (Windows, ignored): the same checks through `StreamEncoder::send_bgra` on a
  D3D11 texture.
- `encoder_software.rs`: NV12 in, AV1 out, forced keyframe (the Windows software path's encoder,
  run on Linux). `encoder_thread_linux.rs`: the real encode loop over stand-in encoders (one that
  fails its trial hands over and the wire's codec byte follows; the last one failing fails the
  stream; a proven one is not replaced), the real thread on a generated capture with a first choice
  that cannot open (frames reach a loopback "relay", nothing is reported on stop), software refused
  above its limits. `bitrate_preset.rs`: option tables.
- Tests that open a real encoder take one lock (`encoder_checks::encoder_lock`): with a dozen
  creating and destroying NVENC sessions in parallel, the NVIDIA driver (620.73.04) crashed in one
  of its own threads (libnvcuvid) about one run in six. The client holds at most a stream's encoder
  and a probe's at once, as it did before.

Results on the development machine (RTX 4080, driver 620.73.04, i7-12700K; 720p30, 3 Mbps, 5 s):

```
NVENC (NVIDIA) h264_nvenc 1280x720@30: 150 packets, 4 keyframes, 3000 kbps of 3000, largest packet 4.0x the average, decoded 150/150, PSNR-Y 28.3 dB (worst 20.4), SSIM 0.961 — OK
NVENC (NVIDIA) hevc_nvenc 1280x720@30: 150 packets, 4 keyframes, 1675 kbps of 3000, largest packet 10.8x the average, decoded 150/150, PSNR-Y 30.0 dB (worst 18.7), SSIM 0.943 — OK
NVENC (NVIDIA) av1_nvenc 1280x720@30: 150 packets, 4 keyframes, 3033 kbps of 3000, largest packet 10.1x the average, decoded 150/150, PSNR-Y 38.1 dB (worst 16.5), SSIM 0.947 — OK
Software (SVT-AV1) libsvtav1 1280x720@30: 150 packets, 4 keyframes, 2245 kbps of 3000, largest packet 3.6x the average, decoded 150/150, PSNR-Y 58.4 dB (worst 52.2), SSIM 1.000 — OK
```

(The test screen is hard on purpose: a full page of small text moving every frame. The low "worst"
values are keyframes squeezed into the four-frame buffer.)

`DECIBELL_ENCODER=vaapi` on this machine (NVIDIA's VA-API driver decodes only) runs the VAAPI open
as far as the driver allows and fails cleanly: device opened, `Unsupported format: bgr0` → CPU tier,
then `No usable encoding entrypoint` for each rate-control mode, reported as
`VAAPI (GPU) h264_vaapi: cannot run here`.

## 8. Not verified, and how to verify it

`crates/decibell-core/tools/verify-encoders.md` has the commands and what success prints.

| What | Status | Check |
|---|---|---|
| VAAPI on AMD (radeonsi) and Intel (iHD): open, GPU convert, CBR, keyframes, decode | compiled and run up to the encoder open on a decode-only driver; never encoded | `DECIBELL_ENCODER=vaapi cargo test --lib forced_backend -- --ignored --nocapture` with the development FFmpeg prefix, then a Go Live |
| Quick Sync on Windows (derive, map, options) | never compiled locally (the Windows Native Check workflow compiles it) | `$env:DECIBELL_ENCODER="qsv"; cargo test --lib forced_backend -- --ignored --nocapture`, then a Go Live |
| AMF under the encoding probe; the Windows probe, start loop, thread fallthrough | never compiled locally | the same with `amf`; a Go Live on AMD |
| Software on Windows (`nv12_readback.rs`, `SoftwarePath`) | never compiled locally; needs an FFmpeg with libsvtav1 (`ffmpeg-encoders.md`) | the same with `software` |
| A live Go Live through NVENC after this change | not driven here (the portal dialog needs a person) | Go Live on the development machine; the log says `[encoder-linux] encoding H264Hw with NVENC (NVIDIA)` |
| Software's limits on a real 4-core laptop | scaled from one desktop CPU | `software_measurements` on such a machine |

## 9. Removed, and left

Removed (superseded, unreferenced): `gpu_interop.rs` (EGL / CUDA / VAAPI DMA-BUF import), the
encoder's CUDA and zero-copy VAAPI constructors and the `cfg(windows)` D3D11 / QSV code inside the
Linux-only `encoder_linux.rs`, the encoder thread's DMA-BUF branch, the `libx264` / `h264_mf`
candidates, the open-only probes.

Left: the capture side's unused DMA-BUF plumbing (`capture::DmaBufFrame`, `gpu_receiver`,
`capture_pipewire.rs`'s `gpu_tx`), `capture_wlr_screencopy.rs`, `codec_selection.rs` (tests only):
capture and negotiation, not this change's ground. The crate's `khronos-egl`, `gl` and `libloading`
dependencies are unused now and stay until the next dependency change (removing them means
re-seeding three lock files).

## 10. For later

- The NVENC / AMF pool on Windows is a fixed pool of render-target textures, which FFmpeg 8.1 turns
  into one texture array (it fails to allocate; the reason vcpkg is pinned). The dynamic pool Quick
  Sync uses here allocates single textures on 8.0 and 8.1 alike: moving NVENC / AMF to it would let
  the pin move, once tried on hardware.
- Dynamic bitrate: only NVENC reconfigures in place, and nothing asks for it yet.
- SVT-AV1 logs its configuration to stderr at every open; a host can set `SVT_LOG=1` before its
  threads start.
