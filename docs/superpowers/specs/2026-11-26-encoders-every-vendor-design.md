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

### NVENC (both OSes)

Linux: BGRA in (the driver converts, with BT.601: §11.2), `preset=p5`, `tune=ull` (`tune=ll
slices=1` for HEVC). Windows: D3D11 NV12, `preset=p4 tune=ull`. Common to both since the quality
work of §11, which has the reason and the measurement for each: `delay=0` (one frame above
1080p60's pixel rate; FFmpeg's default held every stream two frames back), `multipass=fullres`,
`rc=cbr` (`vbr` with the peak at the target for H.264, which otherwise pads with filler),
`forced-idr=1`, `zerolatency=1`, a rate buffer of four frames (at least 133 ms for H.264 / HEVC,
at most 400 ms), and a periodic keyframe every 10 s.

When this section was first written NVENC was left as it was, with one note: `hevc_nvenc` with
`tune=ll` produced 56% of its target bitrate on the test screen (H.264 100%, AV1 101%). §11 is
what came of it.

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
| `rc_buffer_size` = four frames, at most 400 ms | the HRD buffer (NVENC's H.264 / HEVC have a 133 ms floor since §11; VAAPI was not measured and keeps this) |
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
from the stream (10 s since §11; it was 2 s on Linux, 4 s on Windows; a minute for `detail`), buffer of four frames but at
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

- `encoder_bench.rs` (Linux; §11): colours through each backend's conversion and signalling
  (`*_colours_round_trip`: NVENC ×3 and software), the stream contract for NVENC ×3
  (`*_keeps_the_stream_contract`: the bitrate within 10% of the target on five seconds of
  scrolling code, every packet back from its own frame's encode call with that frame's
  timestamp, a requested keyframe on the frame it was requested on, keyframes within the buffer,
  the busiest second within the buffer of the target, everything decodable and at least 36 dB),
  and the bench itself (`quality_bench`, ignored). `encoder_content.rs`: the five generated
  scenes, deterministic and the same at 30 and 60 fps. `bitrate_preset.rs`: the NVENC rules
  (held frames and first pass by pixel rate, the rate buffer per codec, the Linux and Windows
  blocks in step, option spellings).

Results on the development machine as first measured (RTX 4080, driver 620.73.04, i7-12700K;
720p30, 3 Mbps, 5 s; NVENC's PSNR here was taken against BT.709 luma, §11.2):

```
NVENC (NVIDIA) h264_nvenc 1280x720@30: 150 packets, 4 keyframes, 3000 kbps of 3000, largest packet 4.0x the average, decoded 150/150, PSNR-Y 28.3 dB (worst 20.4), SSIM 0.961 — OK
NVENC (NVIDIA) hevc_nvenc 1280x720@30: 150 packets, 4 keyframes, 1675 kbps of 3000, largest packet 10.8x the average, decoded 150/150, PSNR-Y 30.0 dB (worst 18.7), SSIM 0.943 — OK
NVENC (NVIDIA) av1_nvenc 1280x720@30: 150 packets, 4 keyframes, 3033 kbps of 3000, largest packet 10.1x the average, decoded 150/150, PSNR-Y 38.1 dB (worst 16.5), SSIM 0.947 — OK
Software (SVT-AV1) libsvtav1 1280x720@30: 150 packets, 4 keyframes, 2245 kbps of 3000, largest packet 3.6x the average, decoded 150/150, PSNR-Y 58.4 dB (worst 52.2), SSIM 1.000 — OK
```

(The test screen is hard on purpose: a full page of small text moving every frame. The low "worst"
values are keyframes squeezed into the four-frame buffer.)

The same test after §11 (the harness keeps its own keyframe every 2 s; PSNR against the matrix
the stream declares):

```
NVENC (NVIDIA) h264_nvenc 1280x720@30: 150 packets, 4 keyframes, 2875 kbps of 3000, largest packet 3.2x the average, decoded 150/150, PSNR-Y 34.2 dB (worst 20.7), SSIM 0.971 — OK
NVENC (NVIDIA) hevc_nvenc 1280x720@30: 150 packets, 4 keyframes, 2687 kbps of 3000, largest packet 3.4x the average, decoded 150/150, PSNR-Y 36.8 dB (worst 18.3), SSIM 0.947 — OK
NVENC (NVIDIA) av1_nvenc 1280x720@30: 150 packets, 4 keyframes, 2892 kbps of 3000, largest packet 5.2x the average, decoded 150/150, PSNR-Y 41.5 dB (worst 19.9), SSIM 0.972 — OK
Software (SVT-AV1) libsvtav1 1280x720@30: 150 packets, 4 keyframes, 2245 kbps of 3000, largest packet 3.6x the average, decoded 150/150, PSNR-Y 58.4 dB (worst 52.2), SSIM 1.000 — OK
```

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

## 11. Quality: is the picture as good as its bitrate allows

2026-11-26, after §7's table showed `hevc_nvenc` at 56% of its bitrate and all three hardware
results far behind software on the same page. Owner's rule: "Audio and video quality outrank
resource savings in the media path". Everything here was measured on the RTX 4080 (driver
620.73.04, FFmpeg 8.1.3, Linux); what it means for Windows NVENC, AMF, Quick Sync and VAAPI is
§11.9, and what the owner should look at is §11.10.

What was wrong, in the order it matters to a viewer:

1. **Every NVENC stream ran two frames late.** FFmpeg's nvenc wrapper holds a packet until
   `nb_surfaces - 1` (3) frames are queued (`nvenc.c`, `output_ready`; the `delay` option's
   default). Measured from the encode call to the packet, paced in real time: 67.8 ms at 30 fps.
   With `delay=0`: 2.4 ms. The old harness did not see it (it counted packets after a flush).
   The Slides preset over H.264 was worse: its `preset=p7 tune=hq` brought a 28-frame
   look-ahead, more than five seconds at 5 fps (§11.5).
2. **A keyframe every 2 s pulsed the picture.** A keyframe has to fit the rate-control buffer.
   On a still 720p document at 3 Mbps the picture sits at 52 dB, the keyframe comes out at
   19 dB (HEVC) to 24 dB (H.264), and it takes 0.6 s to climb back to 48 dB; then the next
   keyframe is 1.4 s away. Body text stays readable through it, the small print does not, and
   coloured text loses its colour (§11.7).
3. **`hevc_nvenc` gave away its bitrate** in single-pass rate control (§11.3), and
   **`av1_nvenc` ran 21 to 29% over** on grainy video for the same reason.
4. **`h264_nvenc` filled a still screen's whole bitrate with filler data** (88% of every frame).
5. **The colour description was incomplete**, and the harness compared against the wrong matrix
   (§11.2).

### 11.1 The bench

`encoder_bench.rs` (`quality_bench`, ignored) with `encoder_content.rs`. Five kinds of content,
generated (reproducible; nothing of anyone's screen), motion a function of time so 30 and 60 fps
show the same thing:

| Scene | What it is | What it stresses |
|---|---|---|
| `text` | a dark code editor scrolling 240 px/s: 10×14 px glyphs in nine syntax colours (saturated red and blue among them), line numbers, a still tab bar and status bar | thin coloured strokes in motion; rate control under constant change |
| `desktop` | a still light document (dark text, small print, some blue words) with a clock, ten characters a second typed into a box, a blinking caret, a pointer drifting | how a still picture is built up and kept; what a keyframe does to it |
| `slides` | a gradient slide with large and small type and a chart of 1 px coloured lines, replaced every 3 s | a full-picture change, then nothing; banding in gradients |
| `game` | a textured ground panning 300 px/s under twelve shaded sprites, 300 sparks and a HUD | motion at a bitrate that cannot carry all the detail |
| `video` | smooth colour fields drifting, per-pixel grain renewed every frame, a cut every 2.5 s | rate control on content that does not compress; the worst case for overshoot |

Per (scene, size, bitrate, codec) it prints the bitrate, the quietest and busiest second (a
one-second window slid over the stream), the largest keyframe and the 99th-percentile frame
against the average frame and the largest keyframe in seconds of the bitrate (the burst of
datagrams it becomes), the encode call's time, the time from the call to the packet and how many
packets came back after their own call, PSNR of luma and chroma and SSIM against the source
(every third frame; PSNR from the mean squared error over the run), the worst frame, the half
second after each keyframe, and the colour description the decoder read.
`DECIBELL_BENCH_DUMP=<dir>` writes decoded frames beside their source.

The sizes and bitrates are the presets': Data saver 720p30 at 3 Mbps, 1080p30 at 6 Mbps, Smooth
1080p60 at 10 Mbps, Sharp on a 1440p screen (30 fps, 12 Mbps), 1440p60 at 20 Mbps, Slides
(1440p, 5 fps, 12 Mbps, `detail`).

The machine was busy throughout (load average 14 to 57 from other builds). Bitrate, frame sizes
and quality do not depend on that. Times do: they are upper bounds, and where a time decided
something the fastest tenth of the calls (which a busy machine does not inflate) and NVENC's
own utilisation (`nvidia-smi`) were used.

### 11.2 Was the measurement sound

Mostly, and it hid one real fault.

- **NVENC converts BGRA to YUV with BT.601**, studio range, and FFmpeg writes BT.470BG into the
  H.264 / HEVC VUI and the AV1 colour config for packed RGB input whatever the context asks for
  (`nvenc.c`: `if ((pixdesc->flags & AV_PIX_FMT_FLAG_RGB) && !IS_GBRP(...)) vui->colourMatrix =
  to_nv_color_matrix(AVCOL_SPC_BT470BG)`). The code set `colorspace = BT709` and its comment
  said BT.709; the stream said and was BT.601. That pair is consistent: a colour grid encoded
  by each NVENC codec and decoded by FFmpeg's software decoders comes back within 1 level a
  channel when converted with the declared matrix.
- **The harness compared luma against BT.709** whatever the stream declared, so every coloured
  pixel of an NVENC stream counted as an error. On the old test page that cost AV1 3.5 dB
  (38.1 → 41.6 dB), and H.264 and HEVC 0.1 to 0.2 dB (their real loss was far larger). It now
  converts the source with the matrix and range the decoded stream declares, which is what a
  viewer's decoder undoes. The 20 to 30 dB gap to software on that page was real.
- **The description was incomplete**: matrix and range, but primaries and transfer
  "unspecified". Chromium's `VideoColorSpace::IsSpecified()` wants all three, and a decoder
  that ignores a partial description assumes BT.709 for HD: a BT.601 picture decoded as BT.709
  turns pure green (0, 255, 0) into (0, 216, 0) and pure red into (255, 24, 0). Whether the
  shipped Electron build did that was not established here (newer Chromium also guesses the
  missing fields from the present ones); the GPUI client's decoder reads the matrix alone and
  was right either way. Primaries and transfer are now set (BT.709's: an sRGB screen, the
  values every HD decoder assumes), on the Linux encoders and the software encoder, so nothing
  is left to a guess. Windows converts in the D3D11 video processor with BT.709 and declares
  that matrix; its description is still partial but names what a decoder assumes anyway
  (unchanged, §11.9).
- Converting to BT.709 ourselves on the CPU was considered and left: swscale costs 3 to 5 ms a
  1080p to 1440p frame on four threads (§6), and a BT.601 stream that says so is exactly as
  right.

Tests: `*_colours_round_trip` for NVENC ×3 and software (a 4×3 grid: the corners of the RGB
cube, greys, black, near black, near white, white; both byte orders a capture delivers; every
patch within 4 levels of what went in under the declared matrix and range, and the description
complete). `forced_backend_colours_round_trip` is the same for a backend nobody here has.

### 11.3 Why HEVC undershot

720p30, 3 Mbps, `text`, 6 s, a keyframe every 2 s, one option changed at a time from the
settings as shipped (`preset=p5 rc=cbr tune=ll slices=1`, buffer 133 ms):

| Changed | kbps | Quietest second | Busiest second | PSNR-Y dB | Worst frame |
|---|---|---|---|---|---|
| nothing | 2635 | 1710 | 3311 | 34.8 | 23.1 |
| `tune=ull`, `rc=vbr`, `tier=high`, `level=5.1`, `slices` default, `qmin=1`, `ldkfs=4`, `zerolatency=1` (each) | 2635 | 1710 | 3311 | 34.8 | 23.1 (the same bitstream) |
| `tune=hq`; `preset=p4`; `preset=p7` | 2640; 2640; 2616 | | | 34.9; 34.9; 35.0 | 23.1 |
| `spatial-aq=1`; `temporal-aq=1` | 2683; 2627 | | | 35.2; 34.8 | 23.8; 23.1 |
| `cbr_padding=1` | 3020 | 2673 | 3328 | 34.8 | 23.1 (the gap filled with filler) |
| buffer 33 ms (one frame) | 2032 | 1343 | 2442 | 36.5 | 23.0 (one keyframe of 0.41 s of the bitrate) |
| buffer 500 ms; 1 s; 2 s | 2966; 3093; 3308 | 1532; 1520; 1674 | 4188; 5207; 6168 | 42.9; 44.6; 46.6 | 34.4; 35.1; 40.0 |
| `multipass=qres` | 2276 | 1840 | 2688 | 36.0 | 24.8 |
| **`multipass=fullres`** | **2958** | **2683** | **3249** | **36.7** | 25.2 |

So it is not the tier or level (the level NVENC picks carries far more), not a QP clamp, not
the tune (`ll`, `ull` and `hq` give the same bitstream once B-frames and look-ahead are off;
`rc=cbr` and `rc=vbr` with the peak at the target are the same too for HEVC), and not content
that is too easy (the picture is at 35 dB). It is **single-pass rate control against a small
buffer**. Without a first pass the encoder guesses a frame's size from the frames before it. On
a still desktop the trace shows it: the HEVC keyframe came out at 66.9 KB into a 50 KB buffer,
the next frames at 333 bytes to pay it back, and the picture needed a full second to reach
42 dB. Bits a frame does not use are gone (the buffer holds 133 ms and there is no filler), so
every cautious frame lowers the average, while the picture is still poor. With a first pass
over each frame (`multipass=fullres`: inside the same encode call, no frame held) the keyframe
is 37.9 KB, the picture is at 48 dB after 0.6 s, and the stream holds 2958 kbps with its
quietest second at 2683. On the old harness page: 1675 → 2687 kbps and 30.2 → 36.8 dB.

The buffer is the other half: a larger one lets the keyframe be good, which on text carries
through the whole GOP (the scrolled text is predicted from it). That is the `buffer` rows, and
§11.4's decision.

H.264 and AV1, audited the same way:

- **H.264 hit its target by padding.** In CBR `h264_nvenc` writes filler NAL units (type 12) up
  to the bitrate: on the still desktop every 12,500-byte frame held about 11,000 bytes of
  filler, and in a single pass it was already padding 75% of a frame while the picture was
  still at 26 dB. `cbr_padding=0` does not stop it (FFmpeg always asks for HRD timing SEI).
  `rc=vbr` with the peak at the target (`rc_max_rate == bit_rate`, the same buffer) is the same
  constraint without the padding: no filler, 1220 kbps instead of 3000 on the still desktop,
  and on `text` / `game` / `video` the same bitrate (2972 / 3010 / 3009 against 3009 / 3000 /
  3010) and quality (within 0.13 dB).
- **AV1 was the closest to its target on text and the furthest on grain**: 3701 kbps of 3000
  on `video` at 720p30, 7374 of 6000 at 1080p30, 12944 of 10000 at 1080p60, 24242 of 20000 at
  1440p60. With the first pass: within 2%. Its keyframes also overshot the buffer most
  (126 KB into 50 KB on the still desktop; the old harness's "largest packet 10.1× the
  average").

### 11.4 The settings now

NVENC, both OSes (`bitrate_preset.rs`: `NVENC_STREAM`, `nvenc_rc`, `nvenc_sized_opts`,
`rate_buffer_bits`, `nvenc_linux_opts`, `preset_for`; a test keeps the Linux and Windows blocks
in step). Source for every option: `libavcodec/nvenc.c` and `nvenc_{h264,hevc,av1}.c`, FFmpeg
8.1.3 (the names are the same in 8.0.1), and NVIDIA's `nvEncodeAPI.h` 13.0 for what the fields
mean.

| Option | Value | Why |
|---|---|---|
| `delay` | `0` up to 1080p60's pixel rate, `2` above | `0`: the packet comes back from the call that sent the frame (it was two frames later). The call then waits for the upload and the encode: 2.4 ms at 720p, 6.5 ms at 1080p, 10 ms at 1440p, 28 ms at 4K here (queued: 1, –, 4, 14 ms). Above 1080p60 that is more than half the frame interval, and at 1440p120 or 4K60 more than all of it, so there the stream is one frame late (`2`) instead of losing frames. Every preset on a screen up to 1440p gets `0`. |
| `multipass` | `fullres` up to 4K30's pixel rate, `qres` to 4K60, `disabled` beyond | §11.3. It costs NVENC time, not delay: utilisation 15% → 20% at 1440p60 (about 0.8 ms a frame), 21% → 24% at 4K60. `qres` where an older GPU has the least time to spare. |
| `rc` | `vbr` for H.264, `cbr` for HEVC and AV1 (bit rate = max rate either way) | H.264's CBR pads with filler (§11.3) |
| rate buffer | H.264 / HEVC: four frames, at least 133 ms, at most 400 ms. AV1: four frames, at most 400 ms (as before) | 133 ms is what 30 fps streams always had; at 60 fps four frames was 67 ms and keyframes paid for it (1080p60 `text`: +1.4 dB H.264, +1.9 dB HEVC over the stream). Not AV1: with more than four frames its two-pass rate control ran 61% over on 1080p60 `video` and 19% over at 4K60; at four frames it is within 2%. |
| `forced-idr` | `1` | a forced keyframe is an IDR a viewer can join on (as before) |
| `zerolatency` | `1` | no reordering delay (as before on Windows; stated on Linux) |
| `preset` | `p5` Linux, `p4` Windows (as before) | p4 to p7 within 0.3 dB of each other with these settings, in either direction |
| `tune` | `ull`; HEVC on Linux `ll` with `slices=1` (as before) | the tune no longer changes the bitstream; HEVC's stays because of the Chromium decode fault it was chosen for |
| `b_ref_mode` | `disabled` (Windows H.264 / HEVC, as before) | |
| keyframe interval | 10 s (`STREAM_KEYFRAME_SECS`; was 2 s on Linux, 4 s on Windows; `detail` stays 60 s) | point 2 above, and below |
| colour | matrix as converted, studio range, BT.709 primaries and transfer | §11.2 |

Removed: `tile_columns=2`, `tile_rows=1` on Windows `av1_nvenc`. The options are spelled
`tile-columns` / `tile-rows`; the underscore names were never applied, so the bitstream does
not change.

**The keyframe interval** is the one change here that is not an encoder option. Periodic
keyframes are not how a viewer gets a picture: joining, or losing a frame that FEC and NACK do
not recover, sends a keyframe request, repeated every 0.5 s (no keyframe yet) to 1 s
(`video_receiver.rs`), and a requested keyframe is the next frame encoded (tested). The periodic
one is the net under that. Measured on a keyframe every 2 / 4 / 10 / 60 s (720p30, 3 Mbps, 12 s,
PSNR-Y over the run): `desktop` HEVC 30.9 / 33.7 / 35.6 / 38.3 dB, H.264 35.8 / 38.5 / 40.3 /
42.8, AV1 35.5 / 38.2 / 40.0 / 42.6; `text` HEVC 36.9 / 39.3 / 40.8 / 42.3, H.264 39.4 / 41.2 /
42.9 / 44.0, AV1 40.2 / 43.3 / 45.6 / 48.0. 10 s was chosen over 60 s so that a request path
that failed would still heal in seconds; the `detail` streams have run at 60 s since 0.9.3.
One constant, both OSes.

**Keyframe size.** A keyframe leaves as one burst of datagrams at 20 fps and above (the sender
paces only slower streams), so its size is bounded by the buffer: at most the buffer plus the
frame's own share, 0.17 s of the bitrate at 30 fps (measured: 0.01 to 0.17 s). That is no larger,
in time at the stream's bitrate, than what 30 fps streams have always sent; the stream-contract
test fails a keyframe over 0.2 s. With two-pass rate control keyframes now keep to it (single
pass: up to 0.34 s on `desktop`). Larger buffers would buy more on text (table in §11.3) and
are the owner's call: 200 ms would allow 20% over the bitrate in the busiest second and
keyframes of 0.23 s; the 12 Mbps Sharp preset's keyframes would go from about 170 to 250
datagrams (before FEC).

**Intra refresh** (no keyframes at all, a stripe of intra blocks moving down the picture) was
not tried: the receiver starts decoding at a frame flagged as a keyframe and asks for one after
a loss (`video_receiver.rs`), and with intra refresh there is none to send. **4:4:4** would end
the colour bleed on thin coloured text at its root, and is out: Chromium's hardware decoders
and the consumer-GPU decode paths the GPUI client uses take 4:2:0 only.

### 11.5 Before and after

720p30 at 3 Mbps, 1080p30 at 6 Mbps, 1080p60 at 10 Mbps, 1440p60 at 20 Mbps: 6 s a row; 1440p at
5 fps and 12 Mbps (`detail`, as the Slides preset): 20 s. "Before" is the encoder as it was
(single pass, held frames, CBR for all, four frames of buffer, a keyframe every 2 s); "after" is
this section. Quality is measured against the matrix each stream declares.

**Scrolling code (`text`)**

| Size, fps, target | Codec | Bitrate, % of target | Busiest second | Largest keyframe, s of the bitrate | PSNR-Y dB | Worst frame dB | PSNR-C dB | SSIM |
|---|---|---|---|---|---|---|---|---|
| 720p30, 3 Mbps | h264 | 101% → 100% | 110% → 104% | 0.12 → 0.10 | 36.4 → 43.3 | 26.3 → 30.2 | 36.4 → 39.0 | 0.991 → 0.998 |
|  | hevc | 88% → 99% | 110% → 104% | 0.13 → 0.10 | 34.8 → 40.3 | 23.1 → 25.6 | 36.7 → 40.6 | 0.987 → 0.996 |
|  | av1 | 100% → 100% | 114% → 103% | 0.24 → 0.15 | 40.0 → 45.8 | 26.0 → 30.4 | 37.5 → 40.6 | 0.996 → 0.999 |
| 1080p30, 6 Mbps | h264 | 100% → 99% | 110% → 104% | 0.11 → 0.10 | 36.5 → 41.7 | 26.8 → 29.4 | 36.2 → 38.5 | 0.992 → 0.997 |
|  | hevc | 87% → 99% | 111% → 104% | 0.12 → 0.10 | 34.9 → 39.9 | 23.3 → 25.6 | 36.6 → 40.0 | 0.988 → 0.996 |
|  | av1 | 101% → 100% | 111% → 101% | 0.20 → 0.14 | 37.9 → 45.0 | 23.6 → 29.8 | 36.5 → 39.9 | 0.994 → 0.999 |
| 1080p60, 10 Mbps | h264 | 100% → 100% | 105% → 107% | 0.06 → 0.07 | 37.4 → 44.2 | 25.9 → 31.5 | 36.9 → 38.3 | 0.993 → 0.999 |
|  | hevc | 95% → 100% | 104% → 107% | 0.06 → 0.10 | 37.0 → 44.9 | 23.3 → 29.1 | 38.3 → 41.0 | 0.992 → 0.999 |
|  | av1 | 102% → 99% | 109% → 101% | 0.10 → 0.07 | 37.9 → 46.3 | 21.2 → 28.6 | 36.8 → 39.6 | 0.994 → 0.999 |
| 1440p60, 20 Mbps | h264 | 100% → 101% | 104% → 107% | 0.05 → 0.07 | 39.1 → 45.9 | 27.1 → 32.8 | 37.8 → 39.4 | 0.996 → 0.999 |
|  | hevc | 95% → 101% | 104% → 107% | 0.06 → 0.08 | 37.8 → 45.4 | 23.6 → 28.5 | 39.1 → 42.0 | 0.994 → 0.999 |
|  | av1 | 101% → 99% | 107% → 101% | 0.09 → 0.07 | 38.5 → 47.4 | 21.9 → 29.3 | 37.1 → 40.3 | 0.995 → 0.999 |
| 1440p5, 12 Mbps | h264 | 100% → 31% | 118% → 97% | 0.27 → 0.35 | 59.2 → 63.0 | 47.0 → 51.2 | 53.6 → 56.6 | 1.000 → 1.000 |
|  | hevc | 20% → 19% | 64% → 77% | 0.35 → 0.36 | 57.9 → 60.2 | 49.5 → 49.2 | 53.1 → 56.3 | 1.000 → 1.000 |
|  | av1 | 21% → 21% | 119% → 84% | 0.69 → 0.48 | 66.0 → 62.2 | 55.9 → 48.6 | 59.5 → 56.3 | 1.000 → 1.000 |

**Still desktop: clock, typing, pointer (`desktop`)**

| Size, fps, target | Codec | Bitrate, % of target | Busiest second | Largest keyframe, s of the bitrate | PSNR-Y dB | Worst frame dB | PSNR-C dB | SSIM |
|---|---|---|---|---|---|---|---|---|
| 720p30, 3 Mbps | h264 | 100% → 20% | 110% → 73% | 0.12 → 0.10 | 33.6 → 38.7 | 23.2 → 24.3 | 44.0 → 49.6 | 0.992 → 0.997 |
|  | hevc | 32% → 19% | 53% → 74% | 0.18 → 0.10 | 29.8 → 35.3 | 19.5 → 19.8 | 43.5 → 49.2 | 0.984 → 0.995 |
|  | av1 | 77% → 21% | 104% → 77% | 0.34 → 0.17 | 36.0 → 39.6 | 22.1 → 24.4 | 46.3 → 48.8 | 0.997 → 0.999 |
| 1080p30, 6 Mbps | h264 | 100% → 18% | 113% → 95% | 0.12 → 0.10 | 30.8 → 37.1 | 21.7 → 21.8 | 42.8 → 48.7 | 0.986 → 0.997 |
|  | hevc | 36% → 23% | 59% → 88% | 0.19 → 0.10 | 28.0 → 33.0 | 18.2 → 18.1 | 42.8 → 48.3 | 0.978 → 0.993 |
|  | av1 | 72% → 31% | 114% → 89% | 0.30 → 0.15 | 32.4 → 36.0 | 19.7 → 20.8 | 45.2 → 47.9 | 0.994 → 0.997 |
| 1080p60, 10 Mbps | h264 | 100% → 9% | 106% → 50% | 0.07 → 0.10 | 32.9 → 43.0 | 21.1 → 27.5 | 45.0 → 51.8 | 0.991 → 0.999 |
|  | hevc | 26% → 9% | 51% → 50% | 0.10 → 0.10 | 30.2 → 36.6 | 17.8 → 20.7 | 45.0 → 51.3 | 0.986 → 0.997 |
|  | av1 | 58% → 31% | 84% → 66% | 0.16 → 0.08 | 34.0 → 37.5 | 18.4 → 19.5 | 47.4 → 49.4 | 0.995 → 0.998 |
| 1440p60, 20 Mbps | h264 | 100% → 8% | 106% → 43% | 0.06 → 0.10 | 33.5 → 44.3 | 21.4 → 28.7 | 44.9 → 52.2 | 0.993 → 0.999 |
|  | hevc | 23% → 8% | 46% → 44% | 0.09 → 0.10 | 30.8 → 37.8 | 18.0 → 21.6 | 45.3 → 51.6 | 0.988 → 0.998 |
|  | av1 | 48% → 16% | 75% → 55% | 0.13 → 0.08 | 34.4 → 38.5 | 19.1 → 20.3 | 47.2 → 49.8 | 0.996 → 0.999 |
| 1440p5, 12 Mbps | h264 | 100% → 9% | 123% → 57% | 0.22 → 0.41 | 50.7 → 51.7 | 37.6 → 36.6 | 55.1 → 57.2 | 1.000 → 1.000 |
|  | hevc | 4% → 3% | 39% → 51% | 0.34 → 0.37 | 49.9 → 48.2 | 37.8 → 34.2 | 55.2 → 55.5 | 1.000 → 1.000 |
|  | av1 | 6% → 6% | 104% → 73% | 0.92 → 0.59 | 63.0 → 53.6 | 50.1 → 39.0 | 57.6 → 55.5 | 1.000 → 1.000 |

**Slides, a new one every 3 s (`slides`)**

| Size, fps, target | Codec | Bitrate, % of target | Busiest second | Largest keyframe, s of the bitrate | PSNR-Y dB | Worst frame dB | PSNR-C dB | SSIM |
|---|---|---|---|---|---|---|---|---|
| 720p30, 3 Mbps | h264 | 100% → 20% | 110% → 45% | 0.14 → 0.12 | 48.3 → 50.0 | 40.2 → 36.6 | 47.5 → 48.8 | 0.999 → 1.000 |
|  | hevc | 34% → 19% | 49% → 48% | 0.14 → 0.12 | 46.5 → 49.5 | 35.7 → 34.1 | 47.1 → 49.0 | 0.999 → 1.000 |
|  | av1 | 106% → 55% | 146% → 69% | 0.16 → 0.12 | 41.5 → 54.0 | 26.8 → 37.7 | 47.6 → 48.6 | 0.997 → 1.000 |
| 1080p30, 6 Mbps | h264 | 100% → 9% | 108% → 27% | 0.12 → 0.12 | 51.9 → 54.2 | 42.8 → 42.7 | 49.2 → 50.4 | 1.000 → 1.000 |
|  | hevc | 25% → 18% | 41% → 39% | 0.14 → 0.12 | 51.6 → 54.8 | 41.0 → 40.6 | 49.5 → 51.0 | 0.999 → 1.000 |
|  | av1 | 107% → 96% | 131% → 107% | 0.12 → 0.11 | 47.9 → 58.3 | 30.8 → 42.3 | 50.0 → 50.3 | 0.999 → 1.000 |
| 1080p60, 10 Mbps | h264 | 100% → 6% | 105% → 17% | 0.06 → 0.08 | 52.7 → 56.1 | 42.1 → 44.7 | 49.2 → 51.2 | 1.000 → 1.000 |
|  | hevc | 23% → 16% | 32% → 29% | 0.08 → 0.08 | 52.1 → 57.7 | 38.3 → 44.0 | 49.9 → 51.5 | 1.000 → 1.000 |
|  | av1 | 106% → 96% | 118% → 103% | 0.06 → 0.06 | 46.8 → 58.4 | 29.3 → 39.6 | 50.1 → 50.1 | 0.999 → 1.000 |
| 1440p60, 20 Mbps | h264 | 100% → 4% | 103% → 12% | 0.05 → 0.07 | 55.4 → 57.6 | 44.2 → 46.1 | 50.5 → 52.4 | 1.000 → 1.000 |
|  | hevc | 17% → 11% | 24% → 22% | 0.06 → 0.08 | 56.2 → 59.4 | 44.8 → 48.8 | 51.5 → 52.7 | 1.000 → 1.000 |
|  | av1 | 105% → 64% | 114% → 67% | 0.05 → 0.05 | 50.2 → 62.5 | 32.6 → 44.3 | 51.6 → 51.6 | 0.999 → 1.000 |
| 1440p5, 12 Mbps | h264 | 100% → 21% | 133% → 71% | 0.20 → 0.26 | 73.0 → 72.3 | 60.5 → 68.2 | 53.4 → 53.4 | 1.000 → 1.000 |
|  | hevc | 6% → 6% | 16% → 15% | 0.11 → 0.13 | 70.0 → 72.7 | 65.4 → 71.1 | 53.5 → 53.5 | 1.000 → 1.000 |
|  | av1 | 10% → 10% | 32% → 31% | 0.28 → 0.26 | 72.4 → 72.0 | 67.9 → 67.0 | 53.1 → 53.1 | 1.000 → 1.000 |

**Game-like motion (`game`)**

| Size, fps, target | Codec | Bitrate, % of target | Busiest second | Largest keyframe, s of the bitrate | PSNR-Y dB | Worst frame dB | PSNR-C dB | SSIM |
|---|---|---|---|---|---|---|---|---|
| 720p30, 3 Mbps | h264 | 100% → 100% | 107% → 102% | 0.11 → 0.09 | 31.1 → 32.4 | 26.3 → 28.4 | 33.1 → 33.4 | 0.835 → 0.850 |
|  | hevc | 100% → 100% | 110% → 102% | 0.14 → 0.09 | 32.7 → 34.1 | 26.0 → 28.2 | 34.2 → 35.1 | 0.852 → 0.865 |
|  | av1 | 100% → 100% | 111% → 104% | 0.21 → 0.13 | 34.5 → 35.1 | 29.4 → 30.4 | 34.3 → 35.1 | 0.865 → 0.870 |
| 1080p30, 6 Mbps | h264 | 100% → 100% | 108% → 101% | 0.11 → 0.09 | 32.0 → 33.6 | 26.6 → 27.9 | 33.9 → 34.8 | 0.838 → 0.854 |
|  | hevc | 98% → 100% | 110% → 103% | 0.13 → 0.08 | 33.4 → 35.3 | 26.5 → 27.7 | 35.5 → 36.9 | 0.857 → 0.888 |
|  | av1 | 100% → 100% | 111% → 104% | 0.17 → 0.13 | 34.8 → 36.0 | 28.6 → 30.1 | 35.7 → 37.2 | 0.869 → 0.890 |
| 1080p60, 10 Mbps | h264 | 100% → 101% | 105% → 103% | 0.06 → 0.07 | 32.6 → 33.8 | 25.8 → 28.9 | 33.7 → 33.9 | 0.845 → 0.856 |
|  | hevc | 99% → 101% | 105% → 103% | 0.07 → 0.06 | 34.2 → 35.2 | 26.5 → 28.1 | 35.9 → 36.6 | 0.865 → 0.875 |
|  | av1 | 100% → 99% | 107% → 105% | 0.09 → 0.07 | 35.1 → 35.8 | 27.9 → 29.4 | 36.1 → 36.9 | 0.870 → 0.880 |
| 1440p60, 20 Mbps | h264 | 100% → 101% | 105% → 104% | 0.05 → 0.05 | 33.8 → 34.9 | 26.5 → 28.4 | 34.9 → 35.1 | 0.855 → 0.866 |
|  | hevc | 99% → 101% | 105% → 104% | 0.06 → 0.04 | 35.3 → 36.6 | 26.5 → 27.8 | 37.0 → 37.9 | 0.891 → 0.908 |
|  | av1 | 101% → 101% | 108% → 104% | 0.07 → 0.07 | 36.2 → 37.3 | 26.4 → 30.0 | 36.7 → 37.9 | 0.906 → 0.916 |

**Grainy video with cuts (`video`)**

| Size, fps, target | Codec | Bitrate, % of target | Busiest second | Largest keyframe, s of the bitrate | PSNR-Y dB | Worst frame dB | PSNR-C dB | SSIM |
|---|---|---|---|---|---|---|---|---|
| 720p30, 3 Mbps | h264 | 100% → 100% | 109% → 106% | 0.09 → 0.03 | 36.3 → 36.3 | 36.0 → 36.1 | 48.2 → 48.3 | 0.800 → 0.799 |
|  | hevc | 101% → 100% | 104% → 104% | 0.10 → 0.03 | 36.3 → 36.3 | 36.2 → 36.1 | 48.6 → 48.5 | 0.804 → 0.802 |
|  | av1 | 123% → 102% | 129% → 110% | 0.06 → 0.09 | 36.5 → 36.4 | 36.1 → 36.2 | 51.0 → 51.3 | 0.811 → 0.805 |
| 1080p30, 6 Mbps | h264 | 101% → 100% | 108% → 105% | 0.09 → 0.04 | 36.3 → 36.2 | 36.1 → 36.1 | 48.1 → 48.1 | 0.799 → 0.798 |
|  | hevc | 100% → 100% | 105% → 102% | 0.07 → 0.03 | 36.3 → 36.3 | 36.1 → 36.1 | 48.6 → 48.5 | 0.801 → 0.803 |
|  | av1 | 123% → 98% | 128% → 103% | 0.06 → 0.09 | 36.4 → 36.4 | 36.1 → 36.2 | 51.1 → 51.4 | 0.808 → 0.806 |
| 1080p60, 10 Mbps | h264 | 100% → 101% | 103% → 108% | 0.04 → 0.03 | 36.2 → 36.2 | 36.0 → 36.1 | 48.5 → 48.5 | 0.797 → 0.797 |
|  | hevc | 100% → 101% | 105% → 107% | 0.05 → 0.02 | 36.3 → 36.2 | 36.1 → 36.1 | 48.4 → 48.7 | 0.802 → 0.799 |
|  | av1 | 129% → 100% | 133% → 104% | 0.03 → 0.04 | 36.4 → 36.3 | 36.1 → 36.2 | 51.1 → 51.4 | 0.806 → 0.802 |
| 1440p60, 20 Mbps | h264 | 100% → 101% | 104% → 108% | 0.05 → 0.02 | 36.2 → 36.3 | 36.0 → 36.1 | 48.6 → 48.7 | 0.798 → 0.798 |
|  | hevc | 100% → 101% | 104% → 106% | 0.04 → 0.01 | 36.3 → 36.3 | 36.1 → 36.2 | 48.9 → 49.1 | 0.802 → 0.801 |
|  | av1 | 121% → 94% | 127% → 96% | 0.03 → 0.04 | 36.4 → 36.3 | 36.1 → 36.2 | 51.3 → 51.5 | 0.806 → 0.802 |

**Delay** (`text`, paced in real time; ms from the encode call to the frame's packet, p50 / p99;
the machine was busy, so these are upper bounds)

| Size, fps | Codec | Before | After | Frames held, before → after | Encode call, ms p50, before → after |
|---|---|---|---|---|---|
| 720p30 | H.264 / HEVC / AV1 | 68.2 / 70.1, 68.5 / 71.1, 68.4 / 73.5 | 3.4 / 5.1, 3.5 / 5.3, 3.4 / 4.2 | 2 → 0 | 1.5 to 1.8 → 3.4 to 3.5 |
| 1080p60 | H.264 / HEVC / AV1 | 37.3 / 40.8, 37.2 / 39.8, 39.6 / 48.9 | 7.5 / 15.9, 6.8 / 8.8, 7.2 / 11.1 | 2 → 0 | 3.9 to 6.3 → 6.8 to 7.5 |
| 1440p60 | H.264 / HEVC / AV1 | 45.0 / 65.0, 42.5 / 76.5, 41.4 / 54.2 | 23.3 / 30.7, 23.4 / 29.9, 21.9 / 24.2 | 2 → 1 | 7.9 to 10.8 → 5.3 to 6.7 |
| 1440p5 (Slides) | HEVC / AV1 | 408 / 421, 407 / 429 | 23.4 / 25.2, 13.2 / 18.8 | 2 → 0 | 7.9 to 8.4 → 13 to 23 |
| 1440p5 (Slides) | H.264 | no packet in the 6 s run | 12.8 / 16.7 | 28 or more → 0 | 8.6 → 12.8 |

The last row: a `detail` stream is opened with `preset=p7 tune=hq`, and for `h264_nvenc` that
preset brings a 28-frame look-ahead (FFmpeg's log: "Lookahead enabled: depth 28, scenecut
enabled, B-adapt enabled"), which the default `delay` had room for. At 5 fps that is over five
seconds between a slide changing and the viewer seeing it, on both OSes. With `delay=0` FFmpeg
has nowhere to queue the frames and says "Lookahead not enabled. Increase buffer delay" when
the stream opens (a warning in the log, once per stream); HEVC and AV1 never had it.

Reading the quality tables:

- **Bitrate**: on content that moves, every codec is within 2% of its target (HEVC was 87 to
  95% on `text`; AV1 on `video` 121 to 129%), except AV1 on 1440p60 `video`, 6% under. The
  busiest second is at most 10% over (it was up to 33% over for AV1 on `video`, 14% on
  `text`). On `desktop` and `slides` the bitrate falls to what the content needs: H.264 sat on
  its target there because of the filler.
- **`text`**: +5 to +9 dB luma, +1.4 to +3.9 dB chroma. At 720p30 the first pass gives 0.7 to
  3 dB of it and the keyframe interval 3.5 to 5.5 dB (the picture is no longer torn down every
  2 s); at 60 fps the buffer adds 1.4 to 1.9 dB for H.264 and HEVC.
- **`desktop`**: +3.5 to +11 dB at 8 to 31% of the target bitrate. The worst frame (the
  keyframe) is as coarse as before at 30 fps; there is one in ten seconds instead of five.
- **`slides`**: +2 to +12.5 dB (AV1 most: its slide changes used to overshoot and then
  starve). At 720p30 the first frame of a new slide is 1.6 to 3.6 dB coarser than before for
  HEVC and H.264 (34.1 and 36.6 dB) and no coarser at the other sizes.
- **`game`, `video`**: +0.6 to +1.9 dB on `game`, nothing on `video` (grain does not compress;
  36.3 dB is what the bitrate buys). The gain there is the bitrate being kept.
- **Largest keyframe**: within the buffer for every codec (AV1's were up to 0.34 s of the
  bitrate against a 0.13 s buffer).
- **Slides preset** (`1440p5` rows; a keyframe a minute before and after): H.264 and HEVC
  gain 2 to 4 dB on `text`. AV1 reads 4 dB lower on `text` and 9 dB lower on `desktop`
  (63.0 → 53.6 dB over the first 20 s): single-pass AV1 opened the stream with a keyframe of
  0.69 to 0.92 s of the bitrate, up to 1.4 MB against a 0.4 s buffer, which is the megabyte
  keyframe the buffer's cap exists to prevent (`encode_util::vbv_bits`); with the first pass
  it is 0.48 to 0.59 s and the picture takes a few more frames to settle. HEVC on `desktop`
  is 1.7 dB lower for the same reason in miniature (its first keyframe: 37.8 → 34.2 dB).

### 11.6 Software, rechecked

The same bench with `DECIBELL_ENCODER=software` (5 s rows, unpaced):

| Size, fps, target | Preset | `text` kbps / PSNR-Y | `desktop` | `game` | `video` | Largest keyframe, s of the bitrate (`desktop`) |
|---|---|---|---|---|---|---|
| 720p30, 3 Mbps | 8 (the rule's choice) | 2601 / 49.2 | 562 / 63.7 | 2998 / 32.2 | 2096 / 36.1 | 0.06 |
| | 10 | 2778 / 34.1 | 1009 / 48.1 | 3003 / 29.7 | 2027 / 35.1 | 0.39 |
| 1080p30, 6 Mbps | 8 | 4694 / 50.3 | 862 / 64.8 | 5841 / 34.5 | 4065 / 36.2 | 0.07 |
| | 10 (the rule's choice) | 5306 / 34.3 | 1760 / 48.0 | 6006 / 32.2 | 3781 / 35.8 | 0.48 |
| 1080p60, 10 Mbps | 8 | 8546 / 49.5 | 1551 / 66.9 | 9856 / 34.9 | 7209 / 36.2 | 0.05 |
| | 10 (the rule's choice) | 8754 / 35.6 | 3975 / 50.8 | 10012 / 32.4 | 7289 / 36.0 | 0.33 |

- The preset rule stands. Preset 8 is 15 dB better than 10 on scrolling text at the same bitrate
  (the screen-content tools) and 3 to 9 dB better than tuned NVENC at 720p30 on a seventh less
  bitrate, and §6 measured why it
  cannot be used wider: 2 to 3 times the CPU, and past the frame interval at 1080p30 on a
  12700K. Nothing in these numbers argues for trading the other way.
- Software never overshoots (it undershoots grain by a third) and holds no frame.
- **Preset 10's keyframes ignore the buffer**: 0.33 to 0.48 s of the bitrate on a still screen
  (360 KB at 1080p30 / 6 Mbps, about 300 datagrams at once) against preset 8's 0.05 to 0.07.
  Not fixed (no SVT-AV1 option was found that bounds a key frame in low delay); the 10 s
  keyframe interval makes it five times rarer on Linux. Worth a look before software is a
  machine's daily path.
- Colours: exact, BT.709, description complete (test).

### 11.7 The decoded frames, looked at

Frames written by `DECIBELL_BENCH_DUMP`, old settings beside new, 720p30 at 3 Mbps, at 200%:

- **`text`, HEVC, frame 60** (the old stream's keyframe): every glyph smeared into its
  neighbours, strokes doubled; the red words have gone pink and the green ones grey (chroma
  quantised away), with colour bleeding along the rows. The new stream at the same frame is not
  told from the source at that zoom: sharp strokes, the reds and blues their own colour, a
  faint softening of the 1 px gaps inside glyphs.
- **`desktop`, HEVC, frame 60** (keyframe): body text readable but blocky, with a grey haze
  around the glyphs; the small print (5×7 px) an unreadable smudge. Three frames later the body
  text is clean and the small print rough; the new stream is clean at both.
- **`game`, H.264**: old and new look alike. Both flatten the ground's grit into smooth
  patches and lose some sparks; sprite edges and the HUD are sharp in both. At 3 Mbps a
  full-screen pan is limited by the bitrate, not the settings (+0.5 dB).
- **`slides`, HEVC, the first frame of a new slide**: large and small type sharp in both; the
  dark end of the gradient shows faint block steps in both, slightly more visible in the new
  stream on that one frame. The run's average on `slides` (§11.5) says it does not last; a
  frame a few frames later was not looked at.

Not judged here: banding on a real display, motion at 60 fps (stills cannot show judder or
smear), and how any of it looks through Chromium's scaler. §11.10.

### 11.8 Tried and left out

| Tried | Result | Decision |
|---|---|---|
| `spatial-aq=1` (strength 4 / 8 / 12) | PSNR-Y between +0.1 and −1.1 dB on `text`, −0.1 to −0.7 on `game`; SSIM within ±0.001 on `text`, +0.001 to +0.004 on `game` | out: no gain that the numbers or the frames show |
| `temporal-aq=1` | within 0.1 dB everywhere | out |
| `preset` p6 / p7 (and p4 on Linux) | within 0.3 dB, either direction | presets left as they were |
| `tune=hq` | the same bitstream as `ull` / `ll` with these settings | left |
| `multipass=qres` everywhere | cheaper, but HEVC at 76% and AV1 at 90% of the bitrate on `text` | only above 4K30's pixel rate |
| `cbr_padding=1` | hits the bitrate with filler | out |
| a one-frame buffer | no bursts at all; 0.9 dB (HEVC) to 6 dB (H.264, AV1) worse on `text` than four frames with a first pass | out |
| buffers of 200 / 300 / 500 ms | `text` at 720p30: HEVC +1.5 / +2.9 / +4.1 dB, H.264 +1.9 / +3.7 / +5.4, AV1 +3.6 / +4.9 / +6.3; busiest second up to 15 / 20 / 37% over | the owner's call (§11.4) |
| converting to BT.709 on the CPU for NVENC | exact either way | out (§11.2) |

### 11.9 The other backends

Shared code changed for them, unverified on their hardware:

- **Windows NVENC** gets the same `delay`, `multipass`, `rc` and buffer through `preset_for`,
  `nvenc_sized_opts` and `rate_buffer_bits`, and the 10 s keyframe interval. The driver and
  FFmpeg logic are the ones measured here; the input path (D3D11 NV12) is not, so the 2.4 to
  28 ms the waiting call takes on Linux (most of it the upload of a CPU frame) should be less
  there. To check: a still desktop over H.264 now sends far less than its bitrate; the stream
  feels two frames snappier; `forced_backend_streams_are_sound` still passes.
- **AMF, Quick Sync, VAAPI, software**: the 10 s keyframe interval and, for software, the
  complete colour description. Their rate control, buffer (four frames) and options are
  untouched. `h264_amf`'s `header_spacing` follows the GOP up to its limit of 1000 frames, which
  a 10 s GOP passes at 120 fps; `Encoder::for_each_packet`'s parameter-set net covers the rest,
  as it already did for `detail` streams.

What to compare on that hardware is in `crates/decibell-core/tools/verify-encoders.md`
("Quality"): the same bench for VAAPI and software on Linux, the equivalent options per
vendor, and what each column should look like.

### 11.10 For the owner's eye

A Go Live from this machine (NVENC), watched from a second client, before and after:

1. **A still window of small text** (a terminal, a code editor, a document with 9 pt print),
   Sharp or Smooth, each codec. Before: a soft pulse every 2 s, most visible on small or
   coloured text. After: steady; one soft half second when a viewer joins, and every 10 s.
2. **Thin coloured text while scrolling** (syntax-highlighted code, red and blue on dark).
   After: strokes hold their colour and edges; look for colour fringes on red and blue and for
   a trail behind the text.
3. **The first second after a keyframe**: have a second viewer join mid-stream while watching
   the first. The picture softens once for everyone and recovers in about half a second.
4. **Gradients** (a slide with a dark gradient, a wallpaper): blocks or bands just after the
   slide changes, gone within a moment.
5. **Fast motion at 60 fps** (Smooth, a game or a scrolled page): smooth, no stutter. This is
   where `delay=0` could cost frames if the encode call did not fit; the log's
   `[encoder-linux]` frame counters say what was sent.
6. **Delay**: a clock or a metronome on the shared screen against the viewer's. About two
   frames less than before (67 ms at 30 fps).
7. **Colours** on Linux NVENC: a pure red, green and blue test image. Green should not be
   darker on the viewer's side, red not orange.
8. **Recovery**: pull the viewer's network for two seconds. The picture should come back within
   about a second of the network (a keyframe request), not wait for the 10 s keyframe.
9. **A still screen over H.264**: the viewer's bitrate readout (or the sender's upload) falls
   far below the target instead of sitting on it.

### 11.11 Not verified

- Everything on Windows (compiled by the Windows Native Check after merge; no hardware here).
- AMF, Quick Sync, VAAPI quality (no hardware): `verify-encoders.md`.
- What Chromium in the shipped Electron did with the partial colour description (§11.2).
- NVENC generations older than the RTX 40 series: `multipass` and `delay=0` cost more there.
  The pixel-rate rules leave about twice the room measured here.
- 4K60 and 1440p120 on Linux: the encode call takes 14 ms a 4K frame even queued (the frame is
  copied from the capture and uploaded), so those sizes were tight before this and still are.
- Encode times: the machine was never idle.
