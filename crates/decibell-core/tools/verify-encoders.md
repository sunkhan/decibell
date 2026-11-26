# Verifying an encoder backend on its hardware

The core encodes screen shares with NVENC (NVIDIA, both OSes), VAAPI (AMD / Intel, Linux), AMF
(AMD, Windows), Quick Sync (Intel, Windows) and, when none works, software SVT-AV1. Only NVIDIA
hardware was on hand when this was written (2026-11-26): **VAAPI, AMF as probed by the new code,
and Quick Sync have never encoded a frame**. Each has one command that pushes a five-second
synthetic screen share through it, by the stream's own code path, and checks the result. Run it
on a machine with that GPU and the answer is on the last lines.

Design: `docs/superpowers/specs/2026-11-26-encoders-every-vendor-design.md`.

## What the test does

`forced_backend_streams_are_sound` (ignored by default; `DECIBELL_ENCODER` picks the backend):
for H.264, HEVC and AV1 in turn it opens the backend exactly as a stream does, encodes 150 frames
of 1280×720 scrolling text at 30 fps and 3 Mbps, asks for a keyframe at frame 77, and requires

- one packet per frame, timestamps increasing, the first packet a keyframe;
- every keyframe to carry its parameter sets (H.264: SPS + PPS in band; HEVC: VPS + SPS + PPS in
  band on Windows, an hvcC description on Linux; AV1: a sequence header OBU);
- a keyframe within 2 frames of the request (3 on Windows);
- the average bitrate between 30% and 115% of the target;
- the whole stream to decode with FFmpeg's software decoder, 150 frames, no errors (and on
  Linux, the pictures to match the source: PSNR / SSIM printed).

A codec the GPU has no encoder for is reported and skipped (AV1 on anything before Intel Arc /
AMD RX 7000 / RTX 40); the test fails if a codec opens and breaks a rule, or if no codec opens.

## Linux: VAAPI (AMD, Intel)

Needs the VA-API driver (`mesa-va-drivers` / `libva-mesa-driver` for AMD, `intel-media-driver`
for Intel; `vainfo` must list `VAEntrypointEncSlice` or `VAEntrypointEncSliceLP` lines) and an
FFmpeg prefix with the VAAPI encoders, `scale_vaapi` and the software decoders. The development
prefix has all of it (10 minutes, no sudo):

```sh
crates/decibell-core/tools/build-ffmpeg-encoders.sh
cd crates/decibell-core
PKG_CONFIG_PATH=~/.cache/decibell-dev/ffmpeg-enc/prefix/lib/pkgconfig CARGO_TARGET_DIR=target-enc \
  DECIBELL_ENCODER=vaapi cargo test --lib forced_backend -- --ignored --nocapture
```

Success looks like this (one line per codec the GPU encodes, then the count):

```
VAAPI (AMD) h264_vaapi 1280x720@30: 150 packets, 4 keyframes, 2990 kbps of 3000, largest packet 5.2x the average, decoded 150/150, PSNR-Y 31.0 dB (worst 19.5), SSIM 0.95 — OK
VAAPI (AMD) hevc_vaapi 1280x720@30: … — OK
VAAPI (AMD) av1_vaapi: cannot run here: Open encoder: Function not implemented      <- no AV1 encoder on this GPU: fine
vaapi: 2 codec(s) verified
test media::encoder_harness::forced_backend_streams_are_sound ... ok
```

Read the log lines above them too; they say which path ran:

- `[encoder/vaapi] GPU convert: 1280x720 AV_PIX_FMT_BGR0 surfaces -> scale_vaapi -> 1280x720 NV12`
  is the intended path. `GPU convert unavailable (…); converting to NV12 on the CPU` means the
  driver refused RGB surfaces or the conversion: the stream still works, please send the reason.
- `encoder opened: h264_vaapi — … input=VAAPI (BGRA upload + VPP convert; rate control CBR)`:
  `rate control VBR` or `chosen by FFmpeg` means the driver refused CBR.

If it fails, the message names the rule (`HEVC keyframe at pts 60 has no hvcC`, `a keyframe
requested at frame 77 did not come within 2 frames`, `4310 kbps … (over +15%)`,
`the output does not decode: …`). That line plus the `[encoder…]` lines is what is needed.

Things to try when it fails:

- `DECIBELL_VAAPI_CONVERT=cpu …` (same command): skips the GPU conversion. If that passes, the
  driver's video processor is the problem, not the encoder.
- `DECIBELL_VAAPI_DEVICE=/dev/dri/renderD129 …`: another render node (a laptop with two GPUs;
  the test picks the boot GPU first, `ls -l /dev/dri/by-path` shows which is which).
- `LIBVA_DRIVER_NAME=i965 …` on old Intel (before Broadwell) where `iHD` has no encoder.

Then the real thing: `DECIBELL_ENCODER=vaapi` in the client's environment and a Go Live; the log
says `[encoder-linux] encoding H264Hw with VAAPI (AMD)`. Check a watcher sees the picture with
the right colours (a red / blue test image: a swapped or washed-out picture is the conversion).

The same command with `DECIBELL_ENCODER=nvenc` or `software` checks those two (both pass on the
development machine; `software` needs the development prefix, the Electron addon's FFmpeg has no
SVT-AV1).

## Windows: Quick Sync (Intel), AMF (AMD), NVENC, software

In a checkout that builds the addon (`electron-client/HANDOFF.md` §6.1: vcpkg FFmpeg
`ffmpeg[nvcodec,amf,qsv]`), from PowerShell:

```powershell
cd crates\decibell-core
$env:DECIBELL_ENCODER = "qsv"      # or "amf", "nvenc", "software"
cargo test --lib forced_backend -- --ignored --nocapture
```

Success:

```
Quick Sync (Intel) h264_qsv 1280x720@30: 150 packets, 3 keyframes, 2985 kbps of 3000, largest packet 4.8x the average, decoded 150/150 — OK
Quick Sync (Intel) hevc_qsv 1280x720@30: … — OK
Quick Sync (Intel) av1_qsv: cannot run here: avcodec_open2 (av1_qsv): …               <- before Arc / 11th gen: fine
qsv: 2 codec(s) verified
test media::encoder::tests::forced_backend_streams_are_sound ... ok
```

`decode check skipped (no libdav1d decoder in this FFmpeg build)` on the AV1 line is expected with
the vcpkg FFmpeg (it has no AV1 software decoder); the bitstream checks still ran.

What to look at for Quick Sync, since none of it has run:

- `[encoder] QSV device and frames derived from the D3D11VA pool` must appear. A failure before
  it (`av_hwdevice_ctx_create_derived(QSV from D3D11VA) failed`) means the oneVPL / Media SDK
  runtime did not start on the adapter: the Intel graphics driver is missing or too old.
- `av_hwframe_map(D3D11 -> QSV)` in a failure is the per-frame mapping (`encoder.rs` module doc).
- On a laptop with an Intel iGPU and an NVIDIA / AMD GPU, the normal order tries NVENC / AMF
  first; `DECIBELL_ENCODER=qsv` is how the iGPU's encoder is exercised there.

`software` on Windows needs an FFmpeg with libsvtav1, which the pinned vcpkg port cannot build
(`tools/ffmpeg-encoders.md`); without it the test prints
`Software (SVT-AV1) libsvtav1: cannot run here: libsvtav1 is not in this FFmpeg build` and fails
with `no codec could be opened`, which is the correct answer for that build.

Then the real thing: a Go Live with `DECIBELL_ENCODER=qsv` (or `amf`) set for the client; the
log says `[stream] encoding H264Hw with Quick Sync (Intel)`.

## Quality: the same table on your hardware

The checks above say a backend works. This says how good its picture is, and it is where NVENC's
settings came from (design record, "Quality": the table for the RTX 4080 is there to compare
with). One command, Linux, about 25 minutes for everything a GPU encodes:

```sh
cd crates/decibell-core
PKG_CONFIG_PATH=~/.cache/decibell-dev/ffmpeg-enc/prefix/lib/pkgconfig CARGO_TARGET_DIR=target-enc \
  DECIBELL_ENCODER=vaapi cargo test --release --lib quality_bench -- --ignored --nocapture
# a quick first look (2 minutes): two kinds of content, one size
PKG_CONFIG_PATH=~/.cache/decibell-dev/ffmpeg-enc/prefix/lib/pkgconfig CARGO_TARGET_DIR=target-enc \
  DECIBELL_ENCODER=vaapi DECIBELL_BENCH_SCENES=text,desktop DECIBELL_BENCH_ROWS=1280x720@30:3000 \
  cargo test --release --lib quality_bench -- --ignored --nocapture
# colours: every backend must return the RGB it was given, under the matrix its bitstream declares
PKG_CONFIG_PATH=~/.cache/decibell-dev/ffmpeg-enc/prefix/lib/pkgconfig CARGO_TARGET_DIR=target-enc \
  DECIBELL_ENCODER=vaapi cargo test --lib forced_backend_colours -- --ignored --nocapture
```

It encodes five generated kinds of screen content (a scrolling code editor, a still desktop with
a clock and a line being typed, slides, a game-like scene, grainy video; nothing of your screen
is captured) at the Go Live presets' sizes and bitrates, in real time, and prints one row each:

```
scene   | size      fps  target | codec | kbps mean (min..max)      | key x  p99 x  key s | call p50/p99  | lat p50/p99   | late | PSNR-Y  PSNR-C  SSIM   worst  after key | colour
text    | 1280x720   30    3000 | hevc  |   2979 (  2755..  3118) |   3.0    2.6   0.10 |  3.50/5.31   |  3.50/5.31   |    0 |  40.27   40.58  0.9964  25.58      0.00  | 5/1/1/1
```

What to read, and what NVENC does on the development machine:

| Column | Good | A problem |
|---|---|---|
| kbps mean | within a few percent of the target on `text`, `game`, `video`; far under it on `desktop` and `slides` (nothing moves, nothing to send) | over the target; or at the target on `desktop` / `slides` (the encoder is padding: filler data, as `h264_nvenc` did in CBR); or far under it on `text` with a low PSNR (rate control gives bits away, as `hevc_nvenc` did in a single pass) |
| min..max | the busiest second no more than the target plus the buffer (13% for NVENC H.264 / HEVC; NVENC measured at most 10%) | a busiest second 20% or more over: the buffer is not being kept |
| key s | at most 0.17 (the 133 ms buffer plus the frame's own share) | much larger: keyframes overshoot the buffer and leave as one burst of datagrams |
| lat, late | lat about the call time and late 0 (NVENC above 1080p60's pixel rate: every frame late by one, on purpose) | late on every row, or lat of two frame intervals and more: the encoder holds frames back (NVENC held two until `delay=0`); every held frame is a frame interval of delay |
| worst against PSNR-Y | the worst frame is a keyframe or a full-picture change, 10 to 20 dB under the run on still text | a worst frame that low on `game` or `video`, or on many frames: with `DECIBELL_BENCH_TRACE=1` every compared frame's size and PSNR is printed |
| colour | `1/1/1/1` (BT.709; NVENC's BGRA path: `5/1/1/1`, BT.601, which is what it converts with) | a `2` (unspecified) anywhere: a decoder has to guess |

`DECIBELL_BENCH_DUMP=/some/dir` also writes four decoded frames per row next to their source
(`.ppm`: the frame before a keyframe, the keyframe, the one after, one mid-GOP). Look at them:
the small print and the coloured words on the keyframe are where an encoder shows. A stream's
periodic keyframe comes every 10 s, so a 6 s row holds only its first: `DECIBELL_BENCH_GOP=2`
puts one every 2 s (the `after key` column then reads the half second after each), and
`DECIBELL_BENCH_SECONDS=12` shows a stream's own.

The settings to compare if a backend does badly (none of them could be run here; names from
FFmpeg's `amfenc_*.c`, `qsvenc*.c`, `vaapi_encode*.c`):

| NVENC, measured | AMF (Windows) | Quick Sync (Windows) | VAAPI (Linux) |
|---|---|---|---|
| `delay=0`: no held frames | usage `lowlatency` / `latency` already; check the `late` idea by watching for a one-frame lag | `async_depth=1` (set) | `async_depth=1` (set) |
| `multipass=fullres`: a first pass sizes each frame | `preencode=1` (pre-encode assisted rate control) | `extbrc=1` with `low_delay_brc=1`; `look_ahead` stays 0 | none in FFmpeg; radeonsi's pre-encode is a bit of `compression_level` |
| H.264 `rc=vbr` with the peak at the target: no filler | `filler_data` is off by default; `enforce_hrd=true` is set, check a still screen's bitrate | CBR pads by design: raise `maxrate` a hair above the bitrate to get VBR and compare | `rc_mode=VBR` (second in the ladder already) against `CBR` on a still screen |
| buffer 133 ms (`rc_buffer_size`) | the same field | the same field; `max_frame_size_i` bounds a keyframe directly | the same field (HRD) |
| preset: p4 to p7 within 0.3 dB | `quality` `speed` / `balanced` / `quality`; `vbaq=1` (H.264 / HEVC) | `preset` `veryfast` .. `medium` | `compression_level` (meaning differs per driver) |
| a keyframe every 10 s (shared: all backends have it now) | | | |

The Windows harness has no quality bench (it needs the Linux encode path); on Windows the check
is a Go Live watched from a second client, with the scenes listed in the design record.

## The probe

What the client will pick on a machine, without streaming:

```sh
cd crates/decibell-core && cargo test --lib the_real_probe -- --nocapture     # Linux
```

prints one line per codec (`H264Hw: VAAPI (Intel) via h264_vaapi`), every backend it skipped
with the reason, and the time the probe took. In the client the same lines are logged as
`[encoder-probe] …` the first time the encoder list is asked for.

## Everything, on the development machine

```sh
cd crates/decibell-core
cargo test --lib                                   # Electron's FFmpeg: NVENC harness runs, software skips
PKG_CONFIG_PATH=~/.cache/decibell-dev/ffmpeg-enc/prefix/lib/pkgconfig CARGO_TARGET_DIR=target-enc \
  cargo test --lib -- --nocapture                  # + SVT-AV1 and the decode checks
# the measurements behind the software presets and limits (design record §6)
PKG_CONFIG_PATH=~/.cache/decibell-dev/ffmpeg-enc/prefix/lib/pkgconfig CARGO_TARGET_DIR=target-enc \
  DECIBELL_BENCH_PRESETS=8,10,12 cargo test --release --lib software_measurements -- --ignored --nocapture
```
