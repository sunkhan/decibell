# Stream playback: what to run on an AMD and on an Intel machine

Everything in `crates/decibell-video` was built and measured on one machine: an RTX 4080 on NVIDIA's proprietary
driver 620, KDE Plasma Wayland. On that machine all three hardware decoders run (Vulkan Video, NVDEC, and VA-API
through the NVDEC-backed VA driver), so every code path has executed. What has **not** run is any of it on Mesa
(RADV / radeonsi, ANV / the Intel media driver). This page is the list of checks for those machines: one command
each, the line to expect, and what a failure means. Design: [`video-design.md`](video-design.md) §0.5, §3.

Linux only. Windows decodes in software into nothing yet (`video-design.md` §8).

## 0. Once per machine

```sh
cd gpui-client
tools/build-ffmpeg.sh            # vendor/ffmpeg (about 3 minutes; needs libva and libdrm headers)
tools/fetch-gpui.sh              # vendor/gpui-src: the pinned GPUI with patches/*.patch
cargo build --release -p decibell-app
```

**The test streams.** `vendor/streams/*.dbv` are 690 MB and not in the repository. Either:

- copy the folder from the NVIDIA machine (`gpui-client/vendor/streams/`, or anywhere with
  `DECIBELL_VIDEO_STREAMS=/path/to/streams`), which is the better choice: the same bitstreams everywhere; or
- generate them there: `tools/streamgen/make-streams.sh` needs NVENC as written. On AMD / Intel build the generator
  (`tools/streamgen/build.sh`) and run it with the VA-API encoder, one line per file, for example
  `vendor/bin/streamgen vendor/streams/1080p60_h264.dbv h264_vaapi 1920 1080 60 10000 30` (the NVENC options are
  ignored; the names the self-tests read are `1080p60_{h264,hevc,av1}.dbv`, `1440p120_hevc.dbv`, `switch_hevc.dbv`).

Without the streams every `video-*` self-test prints `SKIPPED: no test streams` and passes: check that line is
absent.

What the machine has, before anything of ours runs:

```sh
vulkaninfo --summary | grep -E "deviceName|driverInfo"
vulkaninfo | grep -E "VK_KHR_video_(queue|decode_queue|decode_h264|decode_h265|decode_av1)|VK_EXT_(image_drm_format_modifier|external_memory_dma_buf|physical_device_drm|queue_family_foreign)"
vainfo --display drm --device /dev/dri/renderD128 | grep -E "Driver version|H264|HEVCMain |AV1"
```

- No `VK_KHR_video_decode_*` lines: this Mesa has no Vulkan Video decode, or has it switched off. Older Mesa
  releases kept it behind an environment variable (`RADV_PERFTEST=video_decode` for RADV, `ANV_VIDEO_DECODE=1`
  for ANV: from memory, not checked against a Mesa here; the release notes of the installed version say). The
  client then goes straight to VA-API, which is the case check 2 is for. With the variable set, check 1 becomes
  a real test of Vulkan Video there.
- On a laptop with two GPUs, `renderD128` may be the other one: `ls -l /dev/dri/by-path/` says which is which. The
  client itself always opens the render node of the GPU the window is drawn with.

## 1. The machine's own order

```sh
DECIBELL_LOG=info target/release/decibell --selftest video-hw 2>&1 | grep -v zbus
```

Expect, on a machine with Vulkan Video decode:

```
    decoder order: vulkan, vaapi, software
    probed in … ms: H.264 vulkan, H.265 vulkan, AV1 vulkan
    1080p60_h264.dbv: 3 pictures read back from the texture carry their frame's barcode
    1080p60_h264.dbv: 180 frames in, 180 decoded, 18x presented, … 0 dropped; decode p50 … ms …
    …
    H.264 with vulkan: the window's last frame differs from software decode's by 0.000 of 255 per channel
selftest video-hw: PASS
```

and in the log, once: `video: Vulkan Video on the renderer's device (aux family N, shared with the renderer, decode
family M)`. **"shared with the renderer" is expected on Mesa** (one general-purpose queue family) and is the queue
mode that has only run forced on NVIDIA (`DECIBELL_VIDEO_SHARED_QUEUE=1`): this check is its first real run.

Reading the result:

| Line | Means |
|---|---|
| `decoder order: vaapi, software` | The Vulkan driver has no video decode queue. Fine: check 2 is the test. |
| `probed … : H.264 vulkan, H.265 vulkan, AV1 vaapi` (or `software`) | The GPU does not decode AV1 with Vulkan Video (older than RX 6000 / Arc / 11th-gen), and the line before says `AV1 with vulkan: not used, the driver does not decode AV1 with Vulkan Video`. Correct. |
| `<codec> with vulkan: not used, <reason>` for a codec the GPU does decode | A finding: copy the reason. The stream then plays on VA-API, so the test goes on. |
| `differs from software decode's by` more than `1.0` | Wrong colours or a wrong plane: a bug in the copy for that driver. Take a screenshot of `--screen video-test` (below). |
| `N pictures never reached a frame` | Presentation fell behind. Run again on a quiet machine before believing it. |
| a crash, or `VK_ERROR_DEVICE_LOST` in the log | The most useful finding there is. `dmesg | tail -30`, and the same command with `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation` if the layers are installed. |

Then the rest of the suite on that order (shared-queue mode under resize, recovery, the kept frame):

```sh
target/release/decibell --selftest video-idle,video-two,video-sw,video-switch,video-recover,video-order,video-fastpath
```

Every line `PASS`. `video-order` prints the walk, here `walking: vulkan, vaapi, software`, and one
`… failed -> recorded "…: injected decode error"` per hardware decoder; `video-recover` one
`<decoder>: injected device loss: 1 loss noticed … playback resumed` per hardware decoder.

## 2. VA-API alone

```sh
DECIBELL_VIDEO_DECODER=vaapi DECIBELL_LOG=info target/release/decibell --selftest video-vaapi 2>&1 | grep -v zbus
```

Expect:

```
video: VA-API on /dev/dri/renderD12x
    vaapi decodes: H.264, H.265, AV1
video: VA-API surfaces: 1920x1088 coded for 1920x1080, modifier 0x…, luma R8   @ 0 pitch …, chroma GR88 @ … pitch … (one DMA-BUF)
    1080p60_h264.dbv: 180 frames in, 180 decoded, … 0 dropped; …
    H.264 with vaapi: the window's last frame differs from software decode's by 0.000 of 255 per channel
video: VA-API decoder closed: 180 pictures, N surface imports (… ms in all)
selftest video-vaapi: PASS
```

With `DECIBELL_VIDEO_DECODER=vaapi` in the environment the test fails instead of skipping when VA-API decodes
nothing. What to look at:

| Line | Expect on Mesa / Intel | If not |
|---|---|---|
| `VA-API on /dev/dri/renderD…` | The node of the GPU the window is on. | `VA-API: no usable driver on …`: no VA driver for that GPU (`vainfo` above fails the same way). `…does not say which DRM render node…`: the Vulkan driver has neither `VK_EXT_physical_device_drm` nor `VK_EXT_pci_bus_info`; report the driver version. |
| `VA-API surfaces: … modifier 0x…` | Not seen yet; from the drivers' sources: radeonsi a modifier whose top byte is `02` (AMD), Intel `0x100000000000002` (Y-tiled) or `0x100000000000009` (4-tiled, DG2 / Xe), in both cases one DMA-BUF with the chroma plane at an offset and `GR88`. On NVIDIA it reads `0x300000000606014`, a DMA-BUF per plane, chroma `RG88`. Whatever it is: copy the line into the report. | `modifier 0xffffffffffffff` and the test failing with `…exports surfaces without a DRM format modifier`: the VA driver has no modifier support. Software is used. |
| `N surface imports` | Small and independent of the number of pictures (the decoder's pool: 4 to 22). | N close to the picture count: the import cache does not recognise the driver's surfaces (it keys on the DMA-BUF's inode); it still plays, at an import per frame. Report it with the `surfaces:` line. |
| `the Vulkan driver cannot use a plane of the VA driver's surfaces (R8_UNORM …, DRM modifier 0x…)` | Should not appear. | The Vulkan driver refuses to import an `R8` / `R8G8` image with that modifier as a transfer source. This is the one design risk of the path (`video-design.md` §12): report the modifier and both driver versions. Software is used meanwhile. |
| `decode p50` | 1 to 4 ms at 1080p on a recent GPU. | Above 16 ms the stream cannot hold 60 fps: the decoder is slower than expected or `vaSyncSurface` waits long. |
| `vaapi decodes: H.264, H.265` without AV1 | Correct on a GPU without AV1 decode: the missing codec and the driver's reason are printed. | |

The same decoder under recovery and the order walk is part of check 1's `video-recover` / `video-order` wherever
VA-API works.

## 3. Software alone

```sh
DECIBELL_VIDEO_DECODER=software target/release/decibell --selftest video-sw,video-idle,video-two,video-switch
```

All `PASS`, each stream's line ending in `0 dropped`. On a slow CPU `video-two` (1440p120 HEVC + 1080p60 AV1 at
once) may fail in software with frames dropped: that is the machine, and the reason hardware decode comes first.

## 4. By eye

```sh
DECIBELL_VIDEO_STATS=1 target/release/decibell --screen video-test \
  --dbv vendor/streams/1440p120_hevc.dbv,vendor/streams/1080p60_av1.dbv --mirror
```

and once more with `DECIBELL_VIDEO_DECODER=vaapi` in front. Look for:

- **The picture**: a grey gradient left to right with vertical bars scrolling left, a fine checker in the top third,
  soft colour bands, and a row of 16 black / white cells along the top edge that counts up in binary. Smooth motion,
  no tearing, no blocks, no green or magenta cast (swapped or missing chroma), no bright line at the bottom or right
  edge (the coded picture is 1088 rows; only 1080 are copied).
- **Compositing**: rounded corners on both tiles; over each a label and a translucent disc with a white border; the
  first stream a second time in a small bordered player at the bottom right, in step with its tile.
- **Resize the window** while it plays: the picture follows without a freeze or a flash; nothing in the terminal.
- **The terminal**: one line per stream per second. `backend=` is the decoder in use, `dropped=0 errors=0`,
  `presented` grows by the stream's frame rate, `frames_built` stays put while `frames_reused` grows (each picture
  is a present of the frame the renderer kept).
- **Leave it for a minute**, then `dmesg | tail`: no GPU reset, no `amdgpu` / `i915` / `xe` errors.

## 4a. In the app: what the probe found, a stall, the cursor

These need the local stack (or any server) and a second account streaming; on the development machine
`decibell --selftest voice-stall` runs them against the stack. By eye, on any machine:

- **Settings → Codecs** after the app has been open for a second: under "Detected decoders" one line per codec
  ending in the decoder that is really in use (`· Vulkan Video`, `· VA-API`, `· NVDEC`, `· Software (dav1d)` /
  `· Software (FFmpeg)`), and under them one muted line for every decoder that was tried and is not used, with the
  driver's reason. **On AMD / Intel, copy these lines into the report**: they are the probe's whole answer.
  "Refresh codec capabilities" decodes the test frames again (the button says "Refreshing…" for a moment) and the
  list must come back the same. The answer is kept in `<user data>/video-decoders.txt`.
- **The stats overlay** of a watched stream (the chart button over the video): the `Path` row says hardware or
  software, and it must agree with what Settings lists for the stream's codec.
- **A stream that stops**: while watching a friend's stream, have them pull their network cable (or suspend their
  machine) for ten seconds: after 3 s "Reconnecting…" appears over the last picture, in the stage tile, the focused
  view and the mini-player alike; when they are back the picture resumes within a second and the indicator goes.
- **A busy machine**: watch a 120 fps stream while a game or a build takes every core. The stream may stutter but
  must not freeze for seconds at a time (a decoder thread held back for up to 100 ms no longer loses the stream
  to the next keyframe); the overlay's `Queue` may read up to 12 for a moment and `Dropped` should stay where it is.
- **The cursor in fullscreen**: watch a stream fullscreen and rest the pointer on the video: after 2.5 s the
  controls fade and the cursor disappears; the slightest movement brings both back. Outside fullscreen the cursor
  never hides. Under both Wayland and XWayland (`WAYLAND_DISPLAY= decibell`).

## 5. Numbers, if there is time

One measured run per decoder (CPU, memory, wakeups, the hub's counters; `HANDOFF.md` §4 has NVIDIA's):

```sh
tools/streamgen/measure.sh amd-vulkan-1440p120 20 vendor/streams/1440p120_hevc.dbv
DECIBELL_VIDEO_DECODER=vaapi tools/streamgen/measure.sh amd-vaapi-1440p120 20 vendor/streams/1440p120_hevc.dbv
```

## What to send back

The output of checks 1 and 2 as they are (they print the decoder order, the probe's answer, every decoder given up
with its reason, the surface layout and the import count), `vulkaninfo --summary`, `vainfo`, and the Mesa version.
