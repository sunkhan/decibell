# Watching a stream in the GPUI client: design, and the crate built from it

Status: **`crates/decibell-video` is built, tested and measured on Linux / NVIDIA (2026-11-26)**: §0 describes
it and is the crate's design reference. §1–§14 are the research behind it (decision, what GPUI and wgpu offer,
the options, the prototype's measurements); where the crate differs from the prototype the section says so. GPUI pin: Zed `a3ce8d5e` (2026-11-26),
wgpu 30.0.0 at `cddb2f5d` (`Cargo.toml:1017-1018`). Paths starting with `crates/` are in the pin
(`gpui-client/vendor/zed`), line numbers are the unpatched pin's. wgpu paths are in the pinned wgpu checkout
(`~/.cargo/git/checkouts/wgpu-*/cddb2f5`), FFmpeg paths in FFmpeg 8.1.3.

Contents: [0 The crate as built](#0-the-crate-as-built-cratesdecibell-video) · [1 Decision](#1-decision) · [2 What GPUI and wgpu give us](#2-what-gpui-and-wgpu-give-us-answers-from-source) ·
[3 The Linux options](#3-the-linux-options) · [4 The GPUI patch](#4-the-gpui-patch) ·
[5 Threads, queues, sync](#5-threads-queues-and-synchronisation) · [6 Frame pacing](#6-frame-pacing) ·
[7 Several streams, two places, changes, device loss](#7-several-streams-one-stream-in-two-places-changes-device-loss) ·
[8 Windows and macOS](#8-windows-and-macos) · [9 FFmpeg build](#9-the-ffmpeg-build) ·
[10 Vendor matrix](#10-vendor-matrix) · [11 Measurements](#11-measurements) · [12 Open risks](#12-open-risks-and-what-is-not-proven) ·
[13 Building and running](#13-building-and-running) · [14 Files](#14-files)

## 0. The crate as built (`crates/decibell-video`)

### 0.1 What it is

Hardware video decode (H.264, HEVC, AV1) into textures GPUI draws with no CPU copy of the picture, any number
of streams, software decode as the fallback, on the path §1 chose. It depends on GPUI, `decibell-harness` and
`decibell-ui` (its test screen), not on `decibell-core` or `decibell-model`: the app moves the core's
`StreamFrame` into the hub.

| Piece | File | What |
|---|---|---|
| `VideoDevice::install()` | `lib.rs`, `linux/mod.rs` | The one boot call: registers the Vulkan-device hook (§4) before the first window. Costs two idle queues on the device. |
| `VideoHub` | `hub.rs` | `push(key, EncodedFrame)` from the receive thread; one decoder thread per displayed stream; the presenter task; callbacks to the core. |
| `Stream`, `Viewer` | `hub.rs` | A stream handle (stats, picture size) and "this stream is on screen" (RAII). |
| `VideoView` | `view.rs` | The element: backdrop + one scene surface, `Fit::{Contain, Cover, Fill}`, rounded corners. |
| `Packet`, `PacketBytes` | `packet.rs` | An encoded frame's buffer, owned, handed to FFmpeg without a copy. |
| `FrameQueue` | `queue.rs` | The 3-frame hand-over with the keyframe gate (16 frames while a new decoder catches up). |
| `StreamStats`, `StatsSampler` | `stats.rs` | Lock-free counters, histograms, the per-frame trace. |
| `FfDecoder`, `SoftwareDecoder` | `decode.rs` | FFmpeg plumbing shared by both paths; the software decoder (every platform). |
| `GpuDevice`, `SlotRing`, `VulkanDecoder`, `VulkanUploader`, `WgpuUploader` | `linux/` | Vulkan Video on the renderer's device; the slots; software pictures into slots. |
| `CpuBackend` | `cpu.rs` | Software decode into a counting sink: Windows today, and the unit tests. |
| `dbv::{StreamFile, Feeder}` | `dbv.rs` | `.dbv` test streams and a thread that replays one into a hub. |
| screen `video-test`, six self-tests | `screens.rs`, `selftests.rs` | §0.9. |

```rust
decibell_video::VideoDevice::install();                       // boot, before the first window
let hub = VideoHub::global(cx);                               // app context; created on first use
hub.set_callbacks(HubCallbacks { request_keyframe, interest, event });
hub.push(&frame.key, EncodedFrame { codec, keyframe, timestamp, data: Packet::from(frame.data),
                                    description, discontinuity });   // the core's frame sink
let viewer = hub.stream(&key).viewer();                       // held by the view that shows it
VideoView::new(&viewer).fit(Fit::Cover).rounded(theme.radius_md).backdrop(theme.color_bg_darkest)
hub.remove(&key);                                             // the stream ended
hub.capabilities()                                            // [CodecSupport; 3] for the advertised decode caps
```

### 0.2 What exists when

| State | FFmpeg | Threads | Textures | Core delivers frames |
|---|---|---|---|---|
| App running, no stream ever | nothing (static library, no context) | none | none | no |
| Stream known, nobody displays it (`hub.stream(key)`) | nothing | none | none | no (`interest(key, false)`) |
| A `Viewer` exists, no frame yet | nothing | none | none | yes; a keyframe was requested |
| Displayed and receiving | one `AVCodecContext`; with hardware decode the process's `AVHWDeviceContext` | `decibell-video-<n>` | 5 slots (10 plane textures) | yes |
| No frame for 2 s (stall, or ended without `remove`) | freed | exited | the last picture's slot stays on screen, the other 4 freed | yes |
| Last viewer dropped, or `hub.remove(key)` | freed | exited | all freed (§0.6) | no |

The decision for a stream with no visible view: **it is not decoded.** A frame pushed for it is counted
(`unwatched`) and dropped at `push`, and `HubCallbacks::interest(key, false)` lets the app clear the core's
frame-sink flag so the frames stop arriving at all, which is what the renderer does through its preload's
subscriber count. A view that comes back gets a keyframe request and shows nothing (its backdrop) until the
keyframe arrives; moving a stream between two places (grid tile to mini-player) means creating the new viewer
before dropping the old one, and then nothing restarts. Thread names: Linux truncates a thread's name to 15
characters, so `/proc` and `tools/perf` show `decibell-video-` for every decoder thread.

### 0.3 The life of one frame

Hardware path, steady state. "Lock" names every lock taken, "alloc" every heap allocation, "copy" every copy of
frame or picture data.

| Step | Thread | Work | Lock | Alloc | Copy |
|---|---|---|---|---|---|
| 1 | receive | `Packet::from(frame.data)`: the buffer moves into a recycled carrier. FFmpeg needs 64 zeroed bytes after a packet: a buffer that has them (`PacketBytes::zero_padding`) is carried as it is; one that does not is copied once into a recycled padded buffer of ours and returned to its owner at once | carrier free list (pop) | 0 | 0 with a padded buffer; **1 `memcpy` of the encoded frame today** (`packet_copies()`), because the core's `FrameBuf` has no padding yet (§0.10) |
| 2 | receive | `hub.push`: stream looked up by key (its `Arc` cloned, the map unlocked again), viewer count checked, `received` counted | streams map (read, for the lookup) | 0 | 0 |
| 3 | receive | `FrameQueue::push`: stored in one of three inline slots; the decoder thread is woken if it waits. Full queue: the frame is shed, the gate closes until a keyframe, `request_keyframe(key)` (at most one per 500 ms) | queue mutex (a few moves; nothing is dropped or freed under it) | 0 | 0 |
| 4 | decoder | `FrameQueue::pop` | queue mutex | 0 | 0 |
| 5 | decoder | `av_buffer_create` around the carrier (free callback = drop the owner, which returns a pooled buffer to its pool), `avcodec_send_packet` | FFmpeg takes `device_queue_lock` around its decode submission | FFmpeg: 2 (`AVBuffer` + `AVBufferRef`) plus its own reference | 0 (the hwaccel uploads the slice data to the GPU bitstream buffer: FFmpeg's, one pass) |
| 6 | decoder | `avcodec_receive_frame`: an `AVVkFrame` (the decode is submitted, not finished) | | FFmpeg's | 0 |
| 7 | decoder | a free slot is found (`Arc::strong_count == 1`), the copy is recorded into the decoder's one command buffer: decoded image to the slot's luma and chroma images | `AVVkFrame` lock | 0 | 0 |
| 8 | decoder | submit on the aux queue waiting on the frame's timeline semaphore; `vkWaitForFences`: the thread sleeps 1–3 ms (decode + copy) | `device_queue_lock` for the submit call only | 0 | 1 on the GPU (`vkCmdCopyImage` × 2) |
| 9 | decoder | `WgpuSurface::set_frame(slot.frame.clone(), serial)`; counters and histograms; `PresentSignal::notify` (one atomic swap; wakes the presenter only if it is not already pending) | surface mutex (swap of one `Arc`) | 0 | 0 |
| 10 | main | presenter task: for each stream with a new picture, each window showing it gets one `Window::request_present()` | streams map (read), the stream's viewer list | 0 | 0 |
| 11 | main | GPUI's frame: `renderer.draw(retained scene)`: `prepare_surfaces` takes the newest picture of each surface (the draw observer stamps "presented"), one 4-vertex draw per surface, submit, present | surface mutex; `device_queue_lock` around submit and present | GPUI / wgpu's own (§0.8) | 0 |
| 12 | main, a later frame | `on_submitted_work_done`: the frame's references to the pictures drop; a slot whose count is back to 1 is free | | 0 | 0 |

The software path replaces steps 5–8: FFmpeg decodes on up to four slice / tile threads into its own frame;
the planes are copied **once** into a staging buffer that stays mapped for the decoder's life (luma row by row
or in one `memcpy`, chroma interleaved on the way if the decoder gave planar U and V), and the GPU copies it
into a slot (`vkCmdCopyBufferToImage` × 2 on the aux queue, waited for). Same slots, same publish.

The assertion: `crates/decibell-video/tests/alloc.rs` runs the whole pipeline (feeder with recycled buffers,
hub, queue, software decode, publish) under a counting global allocator and requires **0 Rust allocations over
100 steady-state frames** for each codec, in every thread of the process. The GPU paths add no allocation of
ours by construction (one command buffer, one fence, five slots, all reused) and are counted in the real binary
with `tools/streamgen/malloc-count.c` (§0.8).

### 0.4 Queue, gate and keyframes

The queue holds three frames. A decoder that was just opened gets room to catch up: opening one (FFmpeg's
device context, the frames pool, five slots) takes 10–40 ms, several frame intervals at 120 fps, so until it
has published a picture and found the queue empty the queue holds up to 16 frames; without that, 7 of 30
application starts shed frames in the first 40 ms of a 120 fps stream and waited for a second keyframe.

Mirrors `StreamVideoPlayer.tsx`: a decoder starts on a keyframe; a frame that cannot be queued breaks the
chain, so everything up to the next keyframe is refused at `push` and the streamer is asked for one; a keyframe
that arrives when the queue is full replaces what is queued; `EncodedFrame::discontinuity` (the producer shed
a frame) gates the same way. While gated the request is repeated at most every 500 ms. A new codec, or a
keyframe whose description (hvcC / av1C) differs from the open decoder's, opens a new decoder (the renderer's
reconfigure); parameter sets that change in the bitstream are FFmpeg's business (a resolution switch keeps the
decoder: `video-switch`). HEVC as the streamer sends it (length-prefixed NAL units, parameter sets only in the
keyframe's hvcC) is decoded by giving FFmpeg the description as extradata; tested on both paths with
`dbv::write_hvcc_recording`.

### 0.5 Choosing and changing the decode path

`Backend::open` per stream, at its first keyframe: hardware when the device was created by the hook, the
driver answers `vkGetPhysicalDeviceVideoCapabilitiesKHR` for the codec's 8-bit 4:2:0 profile and the hub is
not forced to software (`DECIBELL_VIDEO=software`, `VideoHub::set_force_software`); otherwise software. If the
hardware open fails, the same keyframe opens a software decoder. `hub.capabilities()` reports the same probe
(`Hardware` with the driver's maximum size, `Software`, or `None`), cached per device, with no FFmpeg context
involved; before the first window exists only the software decoders are reported.

| Failure | Detected by | What happens |
|---|---|---|
| Hardware decoder error (`avcodec_*` or a Vulkan call fails, device alive) | the call's result; then `vkDeviceWaitIdle` under the queue lock to ask whether the device is gone | the stream is marked software-only, `StreamEvent::FellBackToSoftware`, gate + keyframe request, the next keyframe opens the software decoder |
| Software decoder error | the call's result | `StreamEvent::DecoderError`, gate + keyframe request, a new decoder at the next keyframe |
| GPU device lost | `VK_ERROR_DEVICE_LOST` from any of our calls, the driver probe above, or `WgpuContext::device_lost()` checked before every decode | the decoder (with its slots and, with the last decoder, the `AVHWDeviceContext`) is dropped, the surface cleared, `StreamEvent::DeviceLost`; a present is requested so that GPUI looks at its device, which is where it recreates its context; frames are shed until `shared_context()` is a live device again; then the next keyframe opens a decoder on the new device (the hook ran again when it was created). Pictures of the old device are never drawn on the new one (patch `0005`). |

Nothing in these paths runs on the UI thread. The injected versions (`Stream::inject(Fault::DecodeError |
Fault::DeviceLost)`, the second through `WgpuContext::simulate_device_loss`) are what `video-recover` runs: the
renderer really abandons its device and creates another, with a decoder thread in the middle of a stream.

### 0.6 Presenting, and freeing

- One present per new picture per window: the decoder thread sets the stream's dirty flag and wakes the
  presenter (an `AtomicWaker`: no channel node per picture); the presenter, a foreground task with no timer, collects
  the windows of the dirty streams' viewers (`Viewer::shown_in`, recorded when `VideoView` paints) and calls
  `request_present` once on each. Two streams in one window that publish within the same turn of the main loop
  share a present.
- A stream that is torn down leaves references to its last pictures inside the renderer (the frames that
  sampled them, until the renderer notices the GPU finished them; its bind-group cache, until its next frame).
  With no stream there may be no next frame, so the presenter runs one sequence per teardown: present, 120 ms,
  `device.poll`, present, 120 ms, `device.poll`. After it `hub.live_textures()` is 0 (`video-idle`).

### 0.7 Stats and trace

`Stream::stats()` reads atomics: codec, path, size, received / decoded / published / presented / replaced
(published but overtaken before a frame was drawn) / dropped, errors, keyframe requests, decoder opens,
fallbacks, device losses, queue length, and p50 / p99 of decode time (packet handed to the decoder → picture
published), arrival → publish latency and the interval between presented pictures (histograms of 16 buckets per
octave: within 3 %). `StatsSampler` turns two reads into input / decoded / presented fps and kbit/s: the
overlay calls it when it repaints. `DECIBELL_VIDEO_TRACE=<file>` keeps the last 32,768 frames' arrival,
decoded, published and presented times per stream and appends them as CSV when the stream's decoder stops.

Recovery involves every window: GPUI moves each window to the new device at its next frame. With two
windows that crashed in NVIDIA's driver until patch `0006` (the lost instance is no longer destroyed); the
self-test makes the app's main window draw after the loss for that reason.

### 0.8 Measurements of the crate

`gpui-client/HANDOFF.md` §4 "Stream playback" has the table (CPU, memory, wakeups per thread, GPU, pacing, on
Wayland and XWayland) and the comparison with the prototype's §11.

### 0.9 Screen, self-tests, tools

- `decibell --screen video-test --dbv a.dbv[,b.dbv] [--mirror]`, with `DECIBELL_VIDEO_STATS=<s>` (counters on
  stderr from a thread), `DECIBELL_VIDEO_TEST_FRAMES=<n>`, `DECIBELL_VIDEO=software`,
  `DECIBELL_VIDEO_SHARED_QUEUE=1` (the single-queue mode of §5.4), `DECIBELL_VIDEO_UPLOAD=wgpu` (the OpenGL
  fallback's uploader on Vulkan), `DECIBELL_VIDEO_FFMPEG_LOG=<level>`.
- `decibell --selftest video-hw,video-idle,video-two,video-sw,video-recover,video-switch`: real window, real
  GPU. They need `vendor/streams` and say so and pass when it is missing.
- `cargo test -p decibell-video`: queue, stats, the hub with software decode (barcodes read back, callbacks,
  description streams, resolution switch), the zero-allocation test. Same streams; skipped without them.
- `tools/build-ffmpeg.sh` (§9), `tools/streamgen/{build.sh, make-streams.sh, streamgen.c, dbvcat.py}`,
  `tools/streamgen/measure.sh` (one measured run), `tools/streamgen/malloc-count.c`.

### 0.10 The core's side (`core` feature)

The crate does not depend on `decibell-core` unless its `core` feature is on (the app turns it on; the
crate's tests and its Windows build then stay free of the core). With it: `VideoHub::push_frame(StreamFrame)`
is the body of the app's frame sink, `Packet: From<FrameBuf>` carries the core's pooled buffer (dropping the
packet's owner returns it to the core's pool), and `decibell_video::enable_frame_pool()` turns the pool on
with the hub's numbers: 40 buffers (8 streams x the 3 queued + 1 decoding + 1 being received) of at most
1 MiB each (the largest frame of the 30 Mbit/s 2160p test streams is 94 KB).

What is still missing for "no copy": `FrameBuf` promises nothing about the bytes after the frame, so every
packet is copied once into a padded buffer (20 KB per frame at 20 Mbit/s and 120 fps: 2.5 MB/s). When the
pool allocates `len + 64` and keeps the tail zeroed (and `FrameBuf` says so), `CoreBuf::zero_padding` in
`core_glue.rs` returns 64 and the copy is gone: that function is the one line to change.

## 1. Decision

**Primary (Linux): FFmpeg's Vulkan Video hwaccel on the `VkDevice` GPUI renders with**, option (a) of the
brief with one change: the renderer samples a GPU copy of each picture, never the decoder's own image.

- gpui_wgpu creates its device through a small hook, so the device also has the `VK_KHR_video_*` extensions
  and a decode queue. FFmpeg gets that device as an `AVVulkanDeviceContext`.
- A decoder thread per stream decodes and then copies the picture, on the GPU (`vkCmdCopyImage`, plane 0 to an
  `R8` image and plane 1 to an `RG8` image), into one of five slots owned by the stream. The slots are ordinary
  wgpu textures as far as the renderer knows.
- GPUI draws a slot as a scene primitive (`PaintSurface`, which exists in the pin but is only drawn on macOS):
  in scene order, clipped by the content mask, with rounded corners, Y'CbCr to R'G'B' and scaling in the
  fragment shader, other elements over it.
- The renderer picks the newest picture when it draws. A new picture asks the window for a present of the
  scene it already has: no layout, no paint, no view is re-rendered.
- No picture data touches the CPU. One code path covers NVIDIA, AMD (RADV) and Intel (ANV); only NVIDIA is
  proven.

**Hardware fallback: NVDEC (FFmpeg `nvdec` hwaccel, CUDA frames) on NVIDIA and VAAPI on AMD / Intel**, each
ending in the same slot textures (CUDA / Vulkan external memory copy; DRM PRIME DMA-BUF import). Designed, not
built: on this machine the primary path needed no fallback. The FFmpeg prefix already has both hwaccels.

**Software fallback: FFmpeg's H.264 / HEVC decoders and dav1d, one upload per picture into the same kind of
slot textures.** Built and run for all three codecs.

**Windows: FFmpeg D3D11VA on gpui_windows' D3D11 device**, drawn by the same scene primitive (design in §8,
not built; the Qt spike proved the decode half of it on this hardware).

Why not sample the decoded image directly (zero copies): §3. The measured cost with the copy, for one 1440p120
stream, is about 4 % of one core with every frame shown, and about 8 % for two (§11): the copy is not where
the cost is.

## 2. What GPUI and wgpu give us (answers from source)

### 2.1 Surfaces in GPUI's scene (question 1)

- The scene has a surface primitive: `PaintSurface { order, bounds, content_mask, image_buffer }`
  (`crates/gpui/src/scene.rs:1169`), a `Primitive::Surface` variant (`:501`), sorted and batched like every
  other primitive (`PrimitiveBatch::Surfaces(Range<usize>)`, `:824`). The payload field `image_buffer` is a
  `CVPixelBuffer` and exists only on macOS / iOS. On Linux and Windows the struct has no payload at all.
- `Window::paint_surface(bounds, image_buffer)` is macOS / iOS only (`crates/gpui/src/window.rs:5676`), and so
  is the `surface()` element (`crates/gpui/src/elements/surface.rs`, every item under `cfg(macos | ios)`).
- Renderers: Metal draws surfaces (`crates/gpui_apple/src/metal_renderer.rs:1237`: two plane textures from the
  pixel buffer through a `CVMetalTextureCache`, 4:2:0 bi-planar full range only). **gpui_wgpu skips them**
  (`crates/gpui_wgpu/src/wgpu_renderer.rs:1545-1547`: "Surfaces are macOS-only ... not implemented by the WGPU
  renderer"), although the pipeline, the bind group layout (uniform + luma texture + chroma texture + sampler)
  and the shader are already there as dead code (`wgpu_renderer.rs:635-682`, `:972-983`;
  `crates/gpui_wgpu/src/shaders.wgsl:1463-1507`, a fixed full-range BT.601 matrix, no rounding). **gpui_windows
  has an empty `draw_surfaces`** (`crates/gpui_windows/src/directx_renderer.rs:845`).
- Nothing else lets an application hand GPUI a texture. `RenderImage` is CPU pixels (`crates/gpui/src/assets.rs:43`,
  frames of the `image` crate) uploaded into the sprite atlas by `paint_image` (`window.rs:5574`): a video frame
  through it is a CPU copy plus an atlas upload per frame. `PaintNativeView` (`scene.rs:1189`) composites a
  platform view *beneath* the window's content and only Metal implements it (`wgpu_renderer.rs:1549`).
- So: the scene side is nearly ready (ordering, clipping, batching), the Linux and Windows draw sides are
  missing, and there is no way to reach the renderer's device. That is the patch (§4).

### 2.2 The wgpu device (question 2)

- **Who creates what.** The first window's `WgpuRenderer::new` creates the `wgpu::Instance`
  (`WgpuContext::instance`, `crates/gpui_wgpu/src/wgpu_context.rs:486`: backends `VULKAN | GL`) and the surface,
  then `WgpuContext::new` picks an adapter by actually configuring the surface on each candidate
  (`select_adapter_and_device`, `:517`) and creates the device with `adapter.request_device`
  (`create_device`, `:437-483`): the only feature asked for is `DUAL_SOURCE_BLENDING`, limits are
  `downlevel_defaults` at the adapter's resolution, `MemoryHints::MemoryUsage`.
- **Shared across windows: yes.** The platform client owns one `GpuContext = Rc<RefCell<Option<WgpuContext>>>`
  (`wgpu_renderer.rs:182`; created at `crates/gpui_linux/src/linux/wayland/client.rs:1037` and
  `x11/client.rs:520`) and passes it to every window's renderer. A device loss replaces it for all windows
  (`WgpuRenderer::recover`, `wgpu_renderer.rs:2174`).
- **Can an application get it: no.** `WgpuContext` has public fields (`device: Arc<wgpu::Device>`, `queue`,
  `adapter`, `instance`, `wgpu_context.rs:13-22`) and gpui_wgpu re-exports `wgpu`, but the `GpuContext` lives in
  the platform client's private state and `PlatformWindow` has no accessor. Nor can an application supply a
  device or change how it is created.
- **What wgpu 30 offers:**
  - A device with extra Vulkan extensions, queues and feature structs: `wgpu::hal::vulkan::Adapter::open_with_callback`
    (`wgpu-hal/src/vulkan/adapter.rs:3093`; the callback gets `CreateDeviceCallbackArgs { extensions,
    device_features, queue_create_infos, create_info }`, `vulkan/mod.rs:1871`), then
    `wgpu::Adapter::create_device_from_hal` (`wgpu/src/api/adapter.rs:77`). wgpu itself only uses the first queue
    create info (family 0, one queue). A device made entirely by the application is also possible
    (`device_from_raw`, `adapter.rs:2649`).
  - Raw handles: `Device::as_hal::<Vulkan>()` gives `raw_device()`, `raw_physical_device()`, `raw_queue()`,
    `queue_family_index()`, `enabled_device_extensions()`, `shared_instance()` (`wgpu-hal/src/vulkan/device.rs:834-860`).
  - (a) Importing a `VkImage`: `vulkan::Device::texture_from_raw(image, desc, drop_callback, TextureMemory::External)`
    (`device.rs:312`) + `wgpu::Device::create_texture_from_hal(hal_texture, desc, initial_state, cleared)`
    (`wgpu/src/api/device.rs:356`). The last two arguments are what makes a foreign image safe: wgpu is told the
    layout the image is in and that its contents are valid, so it neither clears it nor transitions it from
    `UNDEFINED`. There is also `texture_from_dmabuf_fd` (`device.rs:541`, single-plane only).
  - (b) Multi-planar formats: `TextureFormat::NV12` and `P010` behind `Features::TEXTURE_FORMAT_NV12` / `_P010`
    (`adapter.rs:1032`, `:1047`), plane views with `TextureAspect::Plane0` / `Plane1` mapping to `R8Unorm` /
    `Rg8Unorm` (`wgpu-types/src/texture/format.rs:487-490`). wgpu does not enable `samplerYcbcrConversion`
    (the feature struct is added with the bit left false, `adapter.rs:420-428`): planes are sampled as separate
    views and converted by the application's shader. The prototype does not need the NV12 format at all (its
    slots are two single-plane textures).
  - (c) `ExternalTexture` (`Features::EXTERNAL_TEXTURE`): DX12 and Metal only, not Vulkan
    (`wgpu-hal/src/dx12/adapter.rs:486`, `metal/adapter.rs:1205`). Not usable here.
  - Semaphores: `vulkan::Queue::add_signal_semaphore` (`vulkan/mod.rs:1665`) adds a semaphore the *next* submission
    signals. There is nothing to make a wgpu submission *wait* on a foreign semaphore.
- **Presentation and colour.** Present mode is `Mailbox` on Wayland (`wayland/window.rs:587`) and `Fifo` on X11
  (`x11/window.rs:782`, default at `wgpu_renderer.rs:476-479`), `desired_maximum_frame_latency: 2` (`:480`). The
  swapchain format is `Bgra8Unorm` or `Rgba8Unorm`, never an sRGB format (`:401-414`),
  `SurfaceColorSpace::Auto` (`:473`). GPUI therefore writes gamma-encoded (sRGB) values and blends in gamma space;
  no shader applies a transfer function on output. Video is gamma-encoded too, so **the Y'CbCr to R'G'B' matrix
  result is written as is** (the usual desktop treatment: BT.709-encoded video shown as sRGB). Alpha is
  premultiplied or straight depending on the surface's alpha mode; `blend_color` (`shaders.wgsl:388`) handles
  both and the surface shader uses it.
- **Depth.** The renderer draws opaque quads first with depth writes and gives every later batch the depth of
  its position in the scene (`wgpu_renderer.rs:1460-1476`, compare `Greater`, `:833`). A surface drawn in its
  batch is therefore already hidden under opaque quads painted after it: an opaque panel over a video costs no
  video sampling.

## 3. The Linux options

What "zero copy" has to survive: on this driver the decoder's output picture and its reference picture are
**the same image**. FFmpeg's log for all three codecs here is `Decode modes: reuse_dst_dpb`, i.e. the driver
offers only `VK_VIDEO_DECODE_CAPABILITY_DPB_AND_OUTPUT_COINCIDE`, and FFmpeg uses a separate output image only
when a driver lacks that mode (`libavcodec/vulkan_decode.c:1045`). After decoding, that image is in
`VIDEO_DECODE_DPB` layout, cannot be sampled in it, and the *next* decode needs it back in that layout while the
renderer may still be drawing the previous frame. FFmpeg's answer is the `AVVkFrame` protocol
(`libavutil/hwcontext_vulkan.h:296-356`): lock the frame, wait on its timeline semaphore at `sem_value` in your
submission, signal `sem_value + 1`, record the new layout. libplacebo / mpv follow it, so the GPU orders render
and decode. **wgpu cannot**: its submissions cannot wait on a foreign semaphore (§2.2), and it tracks layouts
itself. Sampling the decoder's image through wgpu therefore means layout changes behind FFmpeg's back, racing
the next decode. A copy into an image that only the renderer samples removes the problem; the decoder waits for
its own copy and nobody else waits for anything.

| | (a) Vulkan Video on wgpu's device **(chosen)** | (b) Vulkan Video on its own device | (c) NVDEC / VAAPI | (d) video outside the scene |
|---|---|---|---|---|
| CPU copies | 0 | 0 | 0 | 0 |
| GPU copies | 1 (`vkCmdCopyImage`, same device) | 1 (into an image exported to wgpu's device) | NVDEC: 1 (CUDA to Vulkan). VAAPI: 0 (the DMA-BUF is the texture) | 0 (decoder presents) |
| CPU cost | measured: ~4 % of a core for 1440p120, ~8 % for two | same plus fd / import bookkeeping | NVDEC: similar. VAAPI: an import + image view per frame | lowest for video; a second swapchain to drive |
| Wakeups | decoder thread ~4 per frame, UI ~3 per frame (§11) | same | same | UI thread not woken at all by video |
| Memory | 5 slots per stream (28 MB of VRAM at 1440p) + decoder pool | same + exported images | NVDEC: a CUDA context on top (not measured) | a swapchain per video |
| NVIDIA today | **works: H.264, HEVC, AV1** (driver 620.73) | should work; two devices' worth of driver memory | works (FFmpeg `nvdec`), needs CUDA interop code | subsurfaces work on KWin; sync with the UI is the hard part |
| AMD / Intel | same code (RADV, ANV decode H.264 / HEVC / AV1); needs "shared queue" mode, built and run on NVIDIA only (§5.4) | same, external memory per vendor | VAAPI is the mature path there | same |
| GPUI patch | device hook + queue lock + surface draw | surface draw only | surface draw only (+ DMA-BUF plane import) | none in the renderer, a lot in gpui_linux |
| Sync | decoder waits for its copy (fence); renderer never waits | same, across devices (shared fence or CPU wait) | NVDEC: `cuStreamSynchronize`; VAAPI: implicit DMA-BUF sync | compositor |
| Breaks | nothing | nothing | nothing | overlays over video, rounded corners, clipping in scroll views, the mini-player moving between parents |

- **(a)** wins because it is one path for three vendors, stays on one device (plain image copy, no external
  memory, no file descriptors), and the patch it needs in GPUI is small. Its costs are the device hook and one
  rule the renderer must follow (§5.3).
- **(b)** needs no device hook, but every slot image must be exported from one device and imported into the
  other (`VK_KHR_external_memory_fd`), layouts have to agree across devices by convention, and the driver keeps
  two devices alive. It is the fallback *for (a)'s hook*, not a simpler design: if a future GPUI makes the device
  impossible to hook, (b) keeps the rest of this design unchanged.
- **(c)** is what to build if Vulkan Video misbehaves on some driver. NVDEC: FFmpeg `nvdec` hwaccel gives CUDA
  frames; a slot image allocated with exportable memory is imported into CUDA (`cuImportExternalMemory`) and
  filled with `cuMemcpy2DAsync`: a GPU copy, the same slot textures. VAAPI: `av_hwframe_map` to
  `AV_PIX_FMT_DRM_PRIME`, import each plane as its own image (`R8` / `GR88` with the plane's offset and modifier:
  `VK_EXT_image_drm_format_modifier`; wgpu's `texture_from_dmabuf_fd` does one plane, `device.rs:541`), the
  surface holds the `AVFrame` until the GPU is done with it. VA surfaces can be sampled while they serve as
  references (no layouts there), so no copy.
- **(d)** is rejected. A `wl_subsurface` with its own swapchain is the cheapest possible video, but the stream
  view in this client has GPUI elements over the picture (name plate, controls, the "+N more" tile), rounded
  corners, and a mini-player that floats over other content. All of those would have to be drawn in a second
  surface above the video and kept in step with the first one; on X11 it would be a child window with the same
  problems. GPUI's own
  `PaintNativeView` works that way (view beneath the window content, a hole punched in the scene) and exists
  only for macOS.

## 4. The GPUI patch

`gpui-client/patches/0001-gpui-external-surfaces.patch`, against the pin (`git apply --check` passes). Seven
files, +574 / -46 lines, of which 150 are the new `wgpu_surface.rs`; per-file table in `patches/README.md`.
The crate also needs `0005-gpui-surface-fit-filter-device.patch` (same files, gpui_wgpu only): object-fit
decided by the renderer at draw time, the minification filter, a device on every picture, the draw observer and
`simulate_device_loss`; described in `patches/README.md` §0005 and used as §0 says. What `0001` adds:

**gpui (core), 2 files.**

- `scene.rs`: on non-Apple targets `PaintSurface` gains `corner_radii: Corners<ScaledPixels>` and
  `source: ExternalSurface`. `ExternalSurface(pub Arc<dyn Any + Send + Sync>)` is an opaque handle: the core
  crate knows nothing about wgpu or D3D, each renderer downcasts to its own type.
- `window.rs`:
  - `Window::paint_external_surface(bounds, corner_radii, source)` (non-Apple): inserts the primitive with the
    snapped bounds and the current content mask, exactly as `paint_surface` does on macOS.
  - `Window::request_present()`: sets the existing `needs_present` flag (`window.rs:1292`) and calls
    `platform_window.schedule_frame()`. The frame callback already has the branch this needs: not dirty but
    `needs_present` means `window.present()`, which draws `rendered_frame.scene` again (`window.rs:2068-2110`,
    `:4039`). No element is laid out or painted.

**gpui_wgpu, 5 files.**

- `wgpu_surface.rs` (new): the types an application uses.
  - `WgpuSurfacePlanes { luma, chroma: wgpu::TextureView }`: a long-lived pair of plane views (a slot).
  - `WgpuSurfaceFrame { planes: Arc<WgpuSurfacePlanes>, color: SurfaceColor, in_use: Option<Box<dyn Any + Send + Sync>> }`:
    one picture. `in_use` is dropped when no submission samples the picture any more.
  - `SurfaceColor { matrix: Bt601 | Bt709 | Bt2020, full_range, chroma_offset }` (default BT.709 limited).
  - `WgpuSurface`: what the element paints. `set_frame(frame)` from any thread; the renderer calls `acquire()`
    when it draws. Counters `pictures_drawn()` and `draws()` (the prototype's "presented" numbers).
- `wgpu_renderer.rs`: `PrimitiveBatch::Surfaces` is drawn. Before the pass, `prepare_surfaces` resolves every
  scene surface to its source's newest frame, writes its parameters (bounds, mask, radii, matrix rows, chroma
  offset: 112 bytes at a dynamic offset in one uniform buffer) and finds or creates the bind group for the
  planes (cached per `Arc<WgpuSurfacePlanes>`, dropped when the planes are). In the pass: one 4-vertex draw
  per surface. After the submit, the frames are moved into `queue.on_submitted_work_done`, so `in_use` drops
  when the GPU has finished with them. Every queue use takes `device_queue_lock()` (§5.3).
- `shaders.wgsl`: the surface shader gets the matrix as uniforms (instead of a constant), the chroma offset,
  `quad_sdf` rounding and `blend_color`. The vertex stage already clipped to the content mask.
- `wgpu_context.rs`:
  - `shared_context() -> Option<WgpuContext>`: the device / queue / adapter / instance every window renders
    with (set when the context is created, replaced after a device loss).
  - `set_vulkan_device_hook(hook)`: if set and the adapter is Vulkan, the device is created with
    `open_with_callback` + `create_device_from_hal` and the hook may add extensions, queues and feature structs.
    Same features, limits and memory hints as before otherwise.
  - `device_queue_lock() -> &'static Mutex<()>`.
- `gpui_wgpu.rs`: module and re-export.

What an element does with it (the whole integration, from the prototype's `main.rs`):

```rust
canvas(|_, _, _| (), move |bounds, _, window, _| {
    window.paint_external_surface(fit(bounds, aspect), Corners::all(px(24.0)), ExternalSurface::new(surface.clone()));
})
```

Properties the patch gives, all exercised by the prototype:

- **Scene order and clipping** are GPUI's own (the primitive is sorted, batched and masked like a sprite).
- **Scaling at drawn size**: the fragment shader samples the planes with linear filtering over the quad the
  element covers. A 1440p stream in a 480 px tile shades a 480 px quad. (Minifying by more than 2x with plain
  bilinear sampling aliases; see §12.)
- **Colour**: BT.601 / 709 / 2020, limited or full range, from the decoded frame's own tags, default BT.709
  limited; chroma siting as a sampling offset.
- **One redraw per new picture, none without**: §6.
- **Out of tree instead of a patch?** Not possible at this pin: the renderer skips surfaces and the device is
  unreachable. The patch is written so that upstreaming is plausible: it completes a primitive GPUI already
  has on one platform, with an opaque payload, and the video-specific parts (the hook, the lock) are generic
  device facilities. Until then it is carried as one patch file over a pinned GPUI.

## 5. Threads, queues and synchronisation

### 5.1 Threads

```
receive path (prototype: feedN)        decoder thread (vdecN), one per stream            main thread (GPUI)
whole encoded frame, timestamp,   -->  avcodec_send_packet  (decode submitted on    
keyframe flag; 3-frame queue;          the decode queue) / avcodec_receive_frame
full queue: drop to next keyframe      copy to a free slot on the aux queue, wait
                                       for the copy's fence (the thread sleeps)
                                       surface.set_frame(slot picture)
                                       signal.notify()  ------------------------->  window.request_present()
                                                                                     frame: renderer.draw(retained scene)
                                                                                       acquire newest picture per surface
                                                                                       submit, present
                                                                                     later: on_submitted_work_done
                                                                                       drops the pictures (slot free)
```

- The decoder is opened with `AV_CODEC_FLAG_LOW_DELAY`, one thread, the Vulkan hwaccel chosen in `get_format`:
  one packet in gives one picture out (streams have no reordering). Whole Annex B access units / AV1 temporal
  units go straight into `avcodec_send_packet`; no parser or bitstream filter is involved.
- **The renderer never waits.** A picture is published only after its copy has finished on the GPU, so
  whatever `acquire()` returns can be sampled immediately. A slow or stalled decoder costs the UI nothing.
- **The decoder waits once per frame**, for its copy (which waits on the decode through the frame's timeline
  semaphore). Measured from "packet handed to the decoder" to "picture published": 1.3-2.8 ms at 1440p, 2.7-3.6 ms
  at 2160p. Because the copy completes before the next packet is decoded, the transition of the decoder's
  image to `TRANSFER_SRC` and back never overlaps a decode that uses it as a reference.

### 5.2 Slots

- Per stream, five slots, each a luma image (`R8_UNORM`, picture size) and a chroma image (`R8G8_UNORM`, half
  size) created with raw Vulkan (`SAMPLED | TRANSFER_DST`, shared between the renderer's and the aux queue
  family), moved to `SHADER_READ_ONLY_OPTIMAL` once, then wrapped: `texture_from_raw` +
  `create_texture_from_hal(.., TextureUses::RESOURCE, cleared = true)`. wgpu only ever samples them, so it never
  records a barrier for them; the copy leaves them in the layout wgpu assumes.
- Only the picture is copied (the decoder's image is coded size, e.g. 1920x1088), so the shader needs no crop.
- A slot is free when the decoder holds the only reference to its `Arc<WgpuSurfaceFrame>` (the crate; the
  prototype used a token in `WgpuSurfaceFrame::in_use` and allocated a frame per picture). The Arc is cloned
  into the surface when the picture is published and into every submission that samples it, so a slot stays
  taken while it is the newest picture and while any submitted frame still samples it. Five covers: newest + up to two in flight + one to write, with one spare. If none
  is free the decoded picture is not published (counted as `no_slot`; never seen in any run).
- VRAM: 1.5 bytes per pixel per slot, x5: 16 MB at 1080p, 28 MB at 1440p, 62 MB at 2160p, next to the decoder's
  own pool (reference pictures).
- wgpu destroys a slot's images (through the `drop_callback`) when the last reference is gone and the GPU is
  idle on them, so a resolution change or a closed stream needs no waiting on our side.

### 5.3 Queues, and the one lock

Vulkan queues are externally synchronised. Who submits where (NVIDIA: families 0 graphics+compute+transfer,
2 compute+transfer, 3 decode+transfer):

| Queue | Used by | From thread |
|---|---|---|
| render family, queue 0 | wgpu: every frame's submit and present | main |
| decode family, queue 0 | FFmpeg: one decode submission per frame | decoder threads |
| aux family, queue 0 (compute + transfer, not the renderer's) | FFmpeg: layout setup of new pool images (a few at stream start). Us: one copy per frame | decoder threads |

All of them take **one process-wide lock, `gpui_wgpu::device_queue_lock()`**, around each submission:
gpui_wgpu around `queue.submit`, `queue.present`, and every surface configure / release; FFmpeg through the
`lock_queue` / `unlock_queue` callbacks of `AVVulkanDeviceContext` (the prototype points them at the lock);
our copy through the same callbacks. The lock is held for the submission call only, never across a wait.

Why one lock and not one per queue: when wgpu reconfigures or releases a surface (window resize, close), its
Vulkan backend calls `vkDeviceWaitIdle` (`wgpu-hal/src/vulkan/swapchain/native.rs:433-446`), and the Vulkan
spec requires that no other thread is submitting to *any* queue of the device during that call. Decoder
threads submit continuously, so without a device-wide lock every resize is a spec violation. One mutex is the
smallest fix and its cost is not measurable here (the per-frame critical sections are tens of microseconds).

### 5.4 Devices without a spare compute queue ("shared" mode)

FFmpeg requires a compute + transfer queue family besides the decode family (`libavutil/hwcontext_vulkan.c`,
`vulkan_device_init`: "compute" and "upload" are required), and the copy needs a queue that can transfer.
NVIDIA has a second such family, so nothing but the renderer touches the renderer's queue. A device whose only
compute queue is the renderer's (RADV, as far as its queue families are known here: one general-purpose queue
plus video queues; not checked on hardware) gets **the renderer's own queue** for FFmpeg's setup and for the
copy, which the device-wide lock already makes safe. The prototype
selects this mode automatically when no spare family exists and `DBV_SHARED_QUEUE=1` forces it; forced on
NVIDIA it ran 2 x 1440p120 and the resize stress with the same results and the same CPU cost as the split mode
(§11). It has not run
on an AMD or Intel GPU.

## 6. Frame pacing

(Question 5.) GPUI at this pin is demand-driven on Linux. A Wayland window with nothing to draw is `Parked`:
no frame callback is requested and the main thread sleeps (`FrameLoop`, `crates/gpui_linux/src/linux/wayland/window.rs:755`;
`complete_frame`, `:973`). `schedule_frame` pings the loop out of `Parked` (`:1052`); after a present the window
waits for the compositor's frame callback (`:2050-2090`).

What a notified view costs: `cx.notify()` marks the view **and all its ancestors** dirty (`mark_view_dirty`,
`window.rs:2569`), the next frame runs `Window::draw`: the root view and every dirty view re-render, lay out
and paint; cached siblings replay their primitives (`reuse_paint`, `:4516`); the scene is rebuilt. Doing that
120 times a second per stream is exactly the CPU this design avoids.

The dedicated path, using what the pin already has:

1. The decoder thread calls `PresentSignal::notify()`: an atomic flag plus the wake of a foreground task
   (the prototype sent a channel message; the crate wakes an `AtomicWaker`). If the flag is already set
   (another picture, or another stream, since the main thread last looked) nothing happens: at most one wake
   per turn of the main loop.
2. The task calls `window.request_present()` (patch): `needs_present = true`, `schedule_frame()`.
3. The frame callback sees "not dirty, needs present" and calls `window.present()`, i.e.
   `renderer.draw(&rendered_frame.scene)` (`window.rs:2099-2110`, `:4039`). The scene is the one painted when the
   stream tile last changed; the renderer resolves each surface to the newest picture *now* (§4).
4. On Wayland the next present cannot happen before the compositor's frame callback, so presents are capped
   at the display's refresh rate without any timer of ours; a picture that arrives while one is waiting
   replaces it ("replaced" in §11: 0 or 1 per run, up to 6 on the loaded machine).

Per video frame the main thread therefore does: one channel wake, `request_present`, one `renderer.draw` of
the retained scene (instance buffers rewritten, one render pass, submit, present), and the compositor's frame
callback. Measured (§11): about 2.3 % of a core for the main thread's share at 120 fps, 4.6 % with two 120 fps streams,
about three voluntary wakeups per frame.

Three things to know when building the real stream view on this:

- **`WindowOptions::inactive_frame_interval` defaults to 33 ms** (`crates/gpui/src/platform.rs:2903`): an
  unfocused window is throttled to 30 fps, presents included (`window.rs:1980-2000`). A window that shows a
  stream must set it to `None` (the client's main window does: `crates/decibell-app/src/window.rs`; a pop-out
  stream window must too).
- **Two unsynchronised streams present up to twice per frame interval** (240 presents a second for
  2 x 120 fps on this 280 Hz display). That is correct (each picture is shown when it arrives) and is where
  the second stream's main-thread cost comes from. On a 60 or 120 Hz display the frame callback caps it.
- **Anything that re-renders the tile at video rate defeats the path.** A frame counter or bitrate label that
  updates per frame must not live in a view that `notify`s per frame; update such labels once a second.

Windows: gpui_windows wakes the window procedure on every vsync from a vsync thread
(`crates/gpui_windows/src/platform.rs:432-449`) and the same `on_request_frame` closure decides whether there
is demand, so `request_present` works unchanged there; what an idle tick costs at 280 Hz is for the GPUI
survey to measure.

## 7. Several streams, one stream in two places, changes, device loss

- **Several streams**: one decoder thread, one `WgpuSurface` and five slots each; one `VideoDevice` (FFmpeg
  device context) for the process. NVIDIA has one decode queue, so decode submissions of all streams share it
  under the lock; 2 x 1440p120 decoded and drew every frame (§11). Each stream's draw is one extra 4-vertex
  draw call in the window's single render pass.
- **One stream in two places** (grid tile + mini-player): paint the same `WgpuSurface` in two elements. Two
  scene primitives resolve to the same picture; no second decoder, no reparenting of a persistent player
  (the Electron client's `StreamPipManager` problem does not exist here: the surface is a value, the elements
  are just two places that paint it). Run in the prototype with `--mirror`. Two *windows* should work the same
  way, since the device is shared between windows and each only needs its own `request_present`; not run.
- **Self-preview**: feed the encoder's output packets into a `VulkanStreamDecoder` like any other stream (what
  Electron does). Cheaper and one frame earlier would be to show the captured frame itself; that depends on the
  capture path of the GPUI client (out of scope here), and the surface type does not care where planes come from.
- **Resolution change mid-stream**: in-band parameter sets make FFmpeg re-negotiate (`get_format` again, new
  frames pool); the decoder sees a different picture size and makes new slots, the old ones are freed when the
  renderer lets go. Run: a stream switching 1080p / 1440p / 2160p HEVC every few seconds decoded every packet
  with no error (§11.3). A switch must start on a keyframe (the wire's keyframe flag; the receive path already requests
  one).
- **Codec change mid-stream**: a new `AVCodecContext` (drop the decoder, open another on the same
  `VideoDevice`, keep the `WgpuSurface`): the element and the scene do not change, the last picture stays up
  until the first new one. Built (§0.4: also for a changed description); the reopen path is exercised by the
  recovery tests, a codec switch itself by no test stream.
- **A stream ends**: `hub.remove(key)`: the decoder thread exits, the surface is cleared, the textures are
  freed (§0.6). A stream that only stops sending keeps its last picture and loses its decoder after 2 s. No
  further presents happen (`video-idle`: zero draws and zero presents over 5 s).
- **Device loss**: GPUI detects it and rebuilds its context on the next frame (`WgpuRenderer::recover`,
  `wgpu_renderer.rs:2174`; all windows adopt the new device). Everything of ours on the old device is dead with
  it. The video layer must: notice (any Vulkan call or FFmpeg call failing with device-lost, or
  `shared_context().device` no longer being the device the `VideoDevice` was made from), drop every decoder
  and the `VideoDevice`, build a new `VideoDevice` from the new context (the hook runs again on its own), reopen
  the decoders and ask each streamer for a keyframe. `WgpuSurface`s survive (they hold only frames; call
  `clear()`). **Built in the crate (§0.5) and run by the `video-recover` self-test** with an injected loss:
  GPUI recreates its context, the stream reopens on the new device. A real device loss was seen once in the
  prototype (§12) and not since.

## 8. Windows and macOS

**Windows: what exists and what the follow-up task must do.** `decibell-video` compiles for Windows without
`gpui_wgpu`, ash or the C shim (`cfg(vk)` is Linux only; `cargo check -p decibell-video --features no-gpu`
type-checks that configuration on Linux; the Windows build itself runs only in the Windows Native Check job
and has not run yet): the hub, the queue, the stats and FFmpeg software decode all run, on `CpuBackend`, whose
uploader counts pictures and draws nothing, and `VideoView` paints its backdrop only. That is the
"unimplemented on this platform → software" decision: nothing pretends to be hardware decode
(`capabilities()` reports `Software`). The follow-up task patches `gpui_windows` and adds a Windows backend:

1. **`gpui_windows` (a patch like `0001` + `0005`, DirectX renderer only):**
   `D3d11Surface` with `set_frame(Arc<D3d11SurfaceFrame>, tag: u64)`, `clear()`, `set_draw_observer`,
   `picture_size()` and a crate-private `acquire(&ID3D11Device)` (the exact shape of `WgpuSurface`);
   `D3d11SurfaceFrame { texture: ID3D11Texture2D, array_slice: u32, size: (u32, u32), color: SurfaceColor,
   device: ID3D11Device, in_use }`; `D3d11SurfaceView { surface, fit: SurfaceFit }`; `draw_surfaces` filled in
   (below); accessors `shared_devices() -> Option<DirectXDevices>` (device + immediate context, replaced after
   a device removal) and a `device_context_lock()` the renderer takes around its frame.
2. **A `windows` module in this crate implementing `decode::Backend`** (the trait is the whole interface:
   `open`, `ready`, `simulate_device_loss`, `support`, `live_textures`, `collect`): `open` returns a
   `PictureDecoder` that is `FfDecoder` with `AV_HWDEVICE_TYPE_D3D11VA` on GPUI's device and publishes the
   decoded texture slice itself (no copy, below), or `SoftwareDecoder` with an `Uploader` that writes two plane
   textures (`UpdateSubresource`, or a mapped dynamic texture) and publishes them as a frame.
   `StreamTarget` gains the `D3d11Surface` under `cfg(windows)`, and `Viewer::surface_view` / `VideoView` the
   matching `paint_external_surface` call. Nothing else in the crate changes: `hub.rs`, `queue.rs`, `stats.rs`,
   `decode.rs`, `view.rs` are already platform-neutral.

The renderer side (design; the Qt spike proved the decode half on this hardware). gpui_windows renders with Direct3D 11: one
`ID3D11Device` + immediate context in `DirectXDevices` (`crates/gpui_windows/src/directx_devices.rs:39-43`, created
with `D3D11_CREATE_DEVICE_BGRA_SUPPORT` only, `:145-170`), a `B8G8R8A8_UNORM` composition swap chain
(`directx_renderer.rs:28`, `:1346`), and `draw_surfaces` is an empty function (`:845`). The matching change:

- `gpui_windows::D3d11Surface` / `D3d11SurfaceFrame { texture: ID3D11Texture2D, array_slice: u32, color, in_use }`,
  the D3D twin of `WgpuSurface`; `PaintSurface.source` downcasts to it in `draw_surfaces`.
- Draw: two shader resource views on the NV12 texture slice (`DXGI_FORMAT_R8_UNORM` and `R8G8_UNORM` planes,
  `Texture2DArray` view with `FirstArraySlice = array_slice`), cached per (texture, slice); a surface pipeline in
  `shaders.hlsl` with the same parameters as the WGSL one; one draw per surface.
- Decode: FFmpeg D3D11VA with `AVD3D11VADeviceContext.device` = GPUI's device (an accessor like
  `shared_context()`), `AVD3D11VAFramesContext.BindFlags |= D3D11_BIND_SHADER_RESOURCE`. The immediate context is
  then used from two threads: turn on `ID3D10Multithread::SetMultithreadProtected` (FFmpeg does it for a device
  it is given without lock callbacks) or give FFmpeg lock callbacks that the renderer also takes around its
  frame, the D3D twin of `device_queue_lock`.
- **No copy is needed on Windows.** D3D11 has no image layouts, and a decoded surface that is still referenced
  is simply not reused by FFmpeg's pool: the frame the renderer samples is an `AVFrame` reference held in
  `in_use`. If a driver refuses `BIND_DECODER | BIND_SHADER_RESOURCE` textures, fall back to what the Qt spike
  did and measured (video processor blit NV12 to BGRA at drawn size into a texture GPUI samples as one plane):
  1440p120 at 5.7-7.8 % of a core on this hardware.
- `Window::paint_external_surface`, `ExternalSurface`, `request_present` and the element code are shared with
  Linux as they are (the core half of the patch is already not Linux-specific).

**macOS** needs nothing new: `paint_surface` takes a `CVPixelBuffer` (what VideoToolbox decodes into) and Metal
draws it (`metal_renderer.rs:1237`). Its limits today: full-range 4:2:0 bi-planar only, no corner radii, and no
`request_present`-style "newest at draw time" indirection (a new buffer means a repaint of the element).

## 9. The FFmpeg build

A second prefix, **`gpui-client/vendor/ffmpeg`**, built by **`gpui-client/tools/build-ffmpeg.sh`** (a variant
of `electron-client/native/scripts/build-ffmpeg-linux.sh`; the Electron script and prefix are untouched). The
script pins every source it fetches by version and SHA-256 (FFmpeg 8.1.3, dav1d 1.5.3, SVT-AV1 v4.2.0, nasm
2.16.03, nv-codec-headers n13.0.19.0, Vulkan-Headers v1.4.335; the hashes are those of the archives as
downloaded on 2026-11-26, not checked against upstream signatures), is a no-op when the prefix's stamp matches,
refuses `--enable-gpl` / `--enable-nonfree` / `--enable-version3` and fails if the configured tree's `config.h`
is not "LGPL version 2.1 or later" or lacks a component the client opens by name. The client links this one
FFmpeg for decode (this crate) and encode (`crates/decibell-core`, `ffmpeg-next` with `static`: the Electron
prefix's NVENC / VAAPI encoders plus `scale_vaapi`, the VAAPI encoders' GPU conversion, and **SVT-AV1**, the
software encoder of a machine with no working hardware one: `crates/decibell-core/tools/ffmpeg-encoders.md`).
FFmpeg **8.1.3**, static, LGPL (no `--enable-gpl`, no `--enable-nonfree`), linked through `ffmpeg-sys-next` 8.1
with the `static` feature and `PKG_CONFIG_PATH=gpui-client/vendor/ffmpeg/lib/pkgconfig`. `decibell-app`'s
`build.rs` refuses to build against a prefix without `libsvtav1`, `libdav1d`, `h264` and `hevc`, and
`decibell --selftest encoder-software` encodes with SVT-AV1 and decodes back with this crate's software path.

- **Why static, why a second prefix**: static keeps one self-contained binary and the exact FFmpeg the Vulkan
  code was tested with (the hwaccel is young: distributions ship 7.x to 9.x). The Electron addon's prefix has
  encoders only and is built into a `.node` that must not grow; this one is a superset (same NVENC / VAAPI
  encoders, so the GPUI client can stream from the same libraries) plus decoders.
- **Why 8.1.3 and not the system's 9.1**: `ffmpeg-next` / `ffmpeg-sys-next` 8.1 are what the workspace already
  uses and they bind 8.0 / 8.1. The system FFmpeg 9.1 decodes all three codecs with `-hwaccel vulkan` on this
  driver too (checked with the CLI before building anything).
- **The script fetches and builds, with no sudo**: nasm 2.16.03 (not installed here; without it the software
  decoders and SVT-AV1 lose their SIMD), Vulkan-Headers v1.4.335 (header-only; not installed here; FFmpeg
  `dlopen`s `libvulkan.so.1` at run time), nv-codec-headers n13.0.19.0 (as Electron), meson in a venv, **dav1d
  1.5.3 as a static library** (BSD-2-Clause) and **SVT-AV1 v4.2.0 as a static library** (cmake, PIC, LTO off:
  BSD-3-Clause-Clear with the Alliance for Open Media Patent License 1.0; 4.0+ is needed, it takes a key frame
  request in low-delay mode). Licence files go to `<prefix>/share/licenses`. Needs from the system: a C / C++
  toolchain, make, cmake, curl, tar, python3, ninja, pkg-config, libva and libdrm headers (libdrm is required,
  not probed: the prefix must not differ between machines). `--stage <dir>` builds beside the prefix with
  pkg-config files that already name it, so a prefix other worktrees link is replaced by two renames.

```
./configure --prefix=$PREFIX --pkg-config-flags=--static --extra-cflags=-I$PREFIX/include --extra-ldflags=-L$PREFIX/lib \
  --enable-static --disable-shared --enable-pic \
  --disable-programs --disable-doc --disable-network --disable-autodetect --disable-everything \
  --enable-avcodec --enable-avutil --enable-swscale --enable-avformat --enable-swresample --enable-avfilter \
  --enable-vulkan --enable-ffnvcodec --enable-nvenc --enable-nvdec --enable-vaapi --enable-libdrm \
  --enable-libdav1d --enable-libsvtav1 \
  --enable-decoder=h264,hevc,av1,libdav1d \
  --enable-parser=h264,hevc,av1 \
  --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,av1_frame_split,av1_frame_merge,extract_extradata \
  --enable-hwaccel=h264_vulkan,hevc_vulkan,av1_vulkan \
  --enable-hwaccel=h264_nvdec,hevc_nvdec,av1_nvdec \
  --enable-hwaccel=h264_vaapi,hevc_vaapi,av1_vaapi \
  --enable-encoder=h264_nvenc,hevc_nvenc,av1_nvenc --enable-encoder=h264_vaapi,hevc_vaapi,av1_vaapi \
  --enable-encoder=libsvtav1 \
  --enable-filter=scale,scale_vaapi --enable-protocol=file
```

(Before 2026-11-26's software encoder work the line had no `--enable-avfilter` (libavfilter was built anyway:
FFmpeg builds every library unless told not to), no `--enable-libsvtav1` / `--enable-encoder=libsvtav1` and
only `--enable-filter=scale`. What the new prefix has beyond the old one, listed from its libraries: the
`libsvtav1` encoder and the `scale_vaapi` filter, nothing else; same decoders, hwaccels, parsers, bsfs.)

- `av1` is FFmpeg's native AV1 decoder, which only decodes through a hwaccel; `libdav1d` is the software one
  (`avcodec_find_decoder(AV_CODEC_ID_AV1)` returns dav1d, so the hardware path opens `"av1"` by name).
- The prefix is 9 static libraries (SVT-AV1's is 9.9 MB, 7.2 MB of it code); the prototype binary (GPUI + wgpu + FFmpeg + dav1d, release, with limited
  debug info) is 34.6 MB stripped (316 MB with its debug info). It loads `libva.so.2`, `libva-drm.so.2` and `libdrm.so.2`
  dynamically (FFmpeg's VAAPI support, as in the Electron prefix): present on every desktop distribution, but a
  hard dependency of the binary.
- Everything that touches FFmpeg's Vulkan structs (`AVVulkanDeviceContext`, `AVVkFrame`) is in a C shim
  compiled against the prefix's headers (`crates/decibell-video/src/linux/shim.c`, 95 lines, built by the
  crate's `build.rs` on Linux only), so struct layouts come from the real headers of whatever FFmpeg version
  is linked. The rest (opening decoders, packets, frames) goes through `ffmpeg-sys-next`'s generated bindings.
- Licence: LGPL-2.1+ for FFmpeg as configured; static linking of an LGPL library obliges the client to ship
  what a user needs to relink (object files or source of the client). The repository being public covers it;
  the release notes should say which FFmpeg source and configure line were used.

## 10. Vendor matrix

| | Decode path | H.264 / HEVC / AV1 | State |
|---|---|---|---|
| Linux, NVIDIA (proprietary driver; here 620.73, RTX 4080) | Vulkan Video on GPUI's device, split queues | yes / yes / yes (AV1: RTX 30 and newer) | **built, measured** |
| | NVDEC (CUDA) fallback | yes / yes / yes | designed; FFmpeg prefix has it |
| Linux, AMD (RADV, a Mesa with Vulkan Video decode enabled) | Vulkan Video, shared-queue mode | expected where the GPU decodes the codec | code path built, run only as forced mode on NVIDIA |
| | VAAPI + DMA-BUF fallback | yes / yes / yes | designed; FFmpeg prefix has it |
| Linux, Intel (ANV with Vulkan Video decode) | Vulkan Video, split or shared mode by its queue families | expected where the GPU decodes the codec | as AMD |
| | VAAPI + DMA-BUF fallback | yes / yes / yes | designed |
| Linux, any GPU or none (GL backend, llvmpipe, old drivers) | software: FFmpeg H.264 / HEVC, dav1d | yes / yes / yes | **built**; 1080p60 fine, 1440p120 only H.264 (§11) |
| Windows, NVIDIA / AMD / Intel | D3D11VA on gpui_windows' device, no copy | yes / yes / yes (per GPU generation) | designed (§8); decode half proven by the Qt spike on NVIDIA |
| Windows, no hardware decoder | software, `UpdateSubresource` into two plane textures | yes / yes / yes | designed |
| macOS | VideoToolbox into `CVPixelBuffer`, GPUI's existing `paint_surface` | yes / yes / AV1 on M3+ (dav1d otherwise) | GPUI side exists |

Which path a machine gets is decided at run time, in this order: Vulkan Video (the hook found a decode
family and `av_hwdevice_ctx_init` plus opening the codec succeed), then the vendor fallback, then software. A
codec the GPU cannot decode (AV1 on an older card) falls to software for that stream only.

## 11. Measurements

Machine: i7-12700K, RTX 4080 (driver 620.73.04), CachyOS, KDE Plasma Wayland; the prototype's window (1600x900
logical) on the 2560x1440 display at **279.96 Hz** (`kscreen-doctor -o`; the other display is 1080p at 180 Hz).
Release build. Streams from `tools/streamgen` (the streamer's NVENC settings): 1080p60 at 10 Mbit/s, 1440p120
at 20 Mbit/s, 2160p60 at 30, 2160p120 at 40. Each run: 4 s settle, then 20 s measured by `tools/measure.py`
(whole process: `/proc/<pid>/stat`, `smaps_rollup`, per-thread `status`, `nvidia-smi pmon -s um`). Other agents
were compiling on the machine during the runs, so CPU figures are an upper bound, not a quiet-machine number.

### 11.1 The matrix (final code, device-wide lock in place)

| Run | CPU, % of a core | of which main / decoder threads | Private / Pss, MB | Vol. ctx switches /s (all; main; decoders; feeders; driver) | GPU sm / dec %, MB | Decoded / drawn / replaced | Packet to picture, ms |
|---|---|---|---|---|---|---|---|
| 1080p60 H.264 | **5.4** | 3.7 / 1.3 | 101.8 / 109.9 | 622; 180; 209; 60; 172 | 0.0 / 6.8, 170 | 1681 / 1680 / 1 | 1.70 |
| 1080p60 HEVC | **5.6** | 3.5 / 1.7 | 105.8 / 114.2 | 628; 179; 216; 60; 172 | 0.1 / 2.2, 313 | 1682 / 1681 / 1 | 1.25 |
| 1080p60 AV1 | **5.5** | 3.6 / 1.5 | 94.8 / 103.4 | 641; 178; 230; 60; 171 | 0.0 / 4.1, 211 | 1682 / 1682 / 0 | 2.07 |
| **2 x 1080p60** (H.264 + AV1) | **10.4** | 7.1 / 2.7 | 110.9 / 119.5 | 1187; 359; 565; 120; 142 | 5.3 / 13.1, 314 | 1682 / 1682 / 0 + 1682 / 1681 / 1 | 2.21, 1.81 |
| 1440p120 H.264 | **6.0** | 4.0 / 1.6 | 105.8 / 115.2 | 1163; 358; 543; 120; 142 | 11.6 / 25.8, 196 | 3362 / 3361 / 0 | 2.79 |
| 1440p120 HEVC | **4.4** | 2.3 / 1.7 | 117.4 / 126.9 | 1092; 361; 469; 120; 141 | 8.0 / 11.8, 341 | 3362 / 3362 / 0 | 1.68 |
| 1440p120 AV1 | **4.1** | 2.3 / 1.3 | 99.2 / 108.8 | 1127; 360; 504; 120; 142 | 5.8 / 15.9, 244 | 3362 / 3362 / 0 | 2.11 |
| **2 x 1440p120** (HEVC + AV1) | **8.5** | 4.6 / 3.3 | 122.3 / 131.5 | 2620; 713; 1524; 240; 142 | 13.6 / 26.8, 518 | 3362 / 3362 / 0 + 3362 / 3362 / 0 | 1.33, 2.12 |
| 2160p60 HEVC | **3.8** | 1.8 / 1.6 | 135.2 / 144.4 | 860; 208; 418; 60; 172 | 0.0 / 11.9, 421 | 1681 / 1681 / 0 | 2.72 |
| 2160p120 AV1 | **5.0** | 2.9 / 1.7 | 95.6 / 105.1 | 1584; 360; 961; 120; 142 | 10.0 / 35.2, 336 | 3362 / 3361 / 0 | 3.63 |
| 2 x 1440p120, shared-queue mode (§11.2) | **19.8** | 12.7 / 6.3 | 119.6 / 128.7 | 2530; 706; 1441; 240; 142 | 21.7 / 27.8, 517 | 3364 / 3360 / 4 + 3364 / 3358 / 6 | 1.57, 1.97 |
| software 1080p60 H.264 | **23.6** | 2.1 / 21.1 | 100.7 / 110.0 | 472; 181; 60; 60; 171 | 3.9 / 0.0, 102 | 1681 / 1680 / 1 | 3.63 |
| software 1080p60 AV1 (dav1d) | **38.0** | 1.7 / 4.0 | 105.8 / 115.0 | 4508; 180; 120; 60; 172 | 7.2 / 0.0, 102 | 1681 / 1681 / 0 | 5.23 |
| software 1440p120 H.264 | **64.3** | 4.3 / 59.5 | 114.6 / 122.9 | 743; 360; 120; 120; 141 | 8.3 / 0.0, 110 | 3361 / 3360 / 1 | 4.98 |
| stream ended 2 s in, then idle | **0.0** | 0.1 / 0.0 | 89.0 / 98.4 | 39; 30; 0; 0; 8 | 0.0 / 0.0, 84 | 240 / 238 / 2 | 1.56 |

How to read it:

- **Decoded / drawn / replaced** are totals over the run's 28 s (so 1681 = 60 fps, 3362 = 120 fps), per stream.
  "Decoded" is counted in the decoder thread, "drawn" is the number of distinct pictures that reached a rendered
  frame (counted in the renderer when it resolves a surface), "replaced" = published but overtaken by a newer
  picture before a frame was drawn. In every hardware run **every packet was decoded, no picture was dropped
  for lack of a slot, and the 3-frame queue never overflowed**. At most 1 picture per run was replaced, except in
  the shared-mode row measured under the heaviest load (4 and 6 of 3,364).
- **CPU** is the whole process (GPUI, wgpu, the NVIDIA driver's threads, FFmpeg, the two stand-in feeder
  threads, the 1 Hz counter thread). "main" is the four threads named after the process (the main thread and
  GPUI's helpers), "decoder" the `vdecN` threads. dav1d's worker threads are in neither column, which is why
  the software AV1 row does not add up.
- **Context switches**: voluntary, per second. Per frame and per stream that is about 3 on the main thread,
  3.5-4.5 on the decoder thread (H.264 / HEVC; AV1 at 120 fps: about 8), 1 on the feeder. The driver's own
  threads (`[vkps] Update`, `[vkcf] Analysis`) wake 140-170 times a second in any process with a swapchain.
- **GPU**: `sm` and `dec` are `nvidia-smi pmon`'s per-process utilisation, `fb` its framebuffer memory (which
  moves between runs with what else is on the GPU; the same number is in `--query-compute-apps`).
- **Memory**: 95-135 MB private while playing. The process with the stream ended (decoder and slots still
  alive) is 89 MB, so playing adds 6-46 MB depending on resolution and codec; most of the rest is GPUI, wgpu and
  the NVIDIA driver, which is the GPUI survey's subject, not this one's.

### 11.2 How much to trust the CPU column

The machine was never quiet: other agents were compiling (load average between 2 and 11 during the series
above, the first four rows at 11). The same scenario measured at different moments:

| Scenario | CPU, % of one core, all runs | Quietest |
|---|---|---|
| 1 x 1080p60 (H.264 / HEVC / AV1) | 3.9, 3.1, 2.8 (first series) · 5.4, 5.6, 5.5 (table above, load 11) | **2.8-3.9** |
| 2 x 1080p60 | 4.6 · 10.4 | **4.6** |
| 1 x 1440p120 (H.264 / HEVC / AV1) | 4.3, (the run that lost the device, §12), 4.1 · 6.0, 4.4, 4.1 | **4.1-4.4** |
| 2 x 1440p120 | 7.5 · 8.5 · 9.9 · 12.0 | **7.5** |
| 2 x 1440p120, shared-queue mode | 19.8 (table) · 9.4 · 9.2 (back to back with split runs at 9.9 and 12.0) | **9.2** |

So: one 1440p120 stream costs about 4 % of a core and two cost about 8 %, of which roughly half is the main
thread presenting the retained scene and a third the decoder threads. The 19.8 % shared-mode row is load, not
the mode: run alternately with the split mode right afterwards, the two were indistinguishable.

For orientation (different OS, same CPU and GPU): the Qt spike measured 5.7-7.8 % for one 1440p120 stream and
18.5 % for two on Windows, and Electron's player path 31.5-33.1 % and 52.6 %
(`docs/reviews/2026-11-07-qt-client-spike-report.md`). The owner's target for 2 x 1440p120 is a third of
Electron's CPU; this path is at about a sixth of it, measured on a busy machine.

### 11.3 Checks beyond the matrix

- **Nothing is drawn when no picture arrives.** `--stop-after 5 --seconds 12` (1080p60 AV1): `window_draws` is
  301 at t = 5 s and still 301 at t = 12 s; the idle row above shows 0.0 % CPU for the 20 s after a stream
  ended. (With other windows opening and taking focus during that test, GPUI redrew 4 times in 6 s for the
  activation changes: not video.)
- **Compositing**: `scratch/shots/hevc_window.png` and `mirror_window.png` are captures of the window: the video
  with 24 px rounded corners, a text label and a translucent bordered disc drawn over it by ordinary GPUI
  elements, and (`--mirror`) stream 0 a second time in a floating mini-player overlapping the second stream's
  tile.
- **The right picture, the right colours**: read off the capture, the barcode in tile 0 (120 fps) says frame
  296 and in tile 1 (60 fps) frame 148, 2.47 s after start for both; the mini-player shows frame 296 as well.
  The barcode's white is (255, 255, 255) and its black (0, 0, 0): the limited-range matrix is exact.
- **All three codecs** decode through Vulkan Video at every size tried (table). Two streams of different codecs
  at once share the one decode queue without loss.
- **Resolution change**: `switch_hevc.dbv` (1080p, then 1440p, then 2160p, looping; 20 s): 1200 of 1201 packets
  decoded, 0 errors, 5 pictures replaced.
- **Window resizing while decoding** (`--resize-test`, 25 resizes a second, each a swapchain reconfigure):
  1442 pictures published and 1440 drawn in 12 s (1440p120 HEVC); 8 s runs in both queue modes with the lock,
  no error.
- **X11** (XWayland, `WAYLAND_DISPLAY=`): 1440p120 HEVC, 720 pictures drawn in 6 s.
- **Software fallback** (rows above, and `scratch/shots/software_test.txt`): all three codecs at 1080p60 with
  every frame drawn (H.264 23.6 % of a core, AV1 38 %); 1440p120 H.264 keeps up at 64 %; 1440p120 HEVC and AV1
  do not on four slice / tile threads while the machine was loaded (292 and 443 of 722 frames dropped at the
  queue in 6 s). The upload itself is cheap: the main thread's share stays at 2-4 %.
- **Start-up soak**: 150 starts of 2 s (1440p120 HEVC) with the lock in place: 0 failures, no Xid in the kernel
  log. Then 100 starts of two streams (1440p120 AV1 + 1080p60 H.264) in shared-queue mode: 0 failures.

## 12. Open risks and what is not proven

(Renumbered for the crate. "Was" refers to the prototype's list.)

1. **One device loss in the prototype, cause not established; recovery is now built.** In the prototype's
   first measurement series (before the device-wide lock of §5.3 existed) one run out of about 95 starts lost
   the Vulkan device at its second decoded frame: kernel log `NVRM: Xid 32 ... name=video-proto` ("invalid or
   corrupted push buffer stream"), then `VK_ERROR_DEVICE_LOST` from our fence wait and from FFmpeg's next
   submit. The likeliest cause found by reading: wgpu calls `vkDeviceWaitIdle` when it reconfigures the
   swapchain (which happens at window start-up) while the decoder thread was submitting, which Vulkan forbids;
   the lock removes that. Not proven: 300 forced reconfigures under load *without* the lock did not reproduce
   it. With the lock: 0 losses in the prototype's 250 soak starts and about 40 measurement runs, and 0 in the
   crate's runs (HANDOFF §4's two matrices, the self-tests in every mode, and 100 two-stream application
   starts of 3.5 s: no device loss, no decoder error; the 37 of those starts that shed frames did so because
   of the queue rule fixed in §0.4, and the last 30, with the fix, shed none). The crate
   treats a loss as something that happens (§0.5); what remains unproven is recovery from a *real* loss (the
   injected one leaves the old device usable while it is torn down; after a real one every call on it fails,
   which the code handles by the same path but no test can produce).
2. **No validation layers were available** (`vulkan-validation-layers` is not installed and nothing was
   installed system-wide), so the raw Vulkan in the copy, upload and read-back paths and the
   FFmpeg-on-foreign-device setup have only been checked against the spec by reading, and against one driver by
   running. First thing to do on a machine with the layers: run the self-tests with
   `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`.
3. **AMD and Intel are unproven.** Specifically unknown there: whether RADV / ANV accept `vkCmdCopyImage` from a
   decode-profile image on the general queue (the spec allows it when the image has `TRANSFER_SRC` usage, which
   FFmpeg sets, `vulkan_decode.c:1221`), and the shared-queue mode's behaviour on a real single-queue device
   (forced on NVIDIA with `DECIBELL_VIDEO_SHARED_QUEUE=1` it passes the self-tests). The NVDEC / VAAPI
   fallbacks of §3 (c) are not built: on such a machine a failing Vulkan decoder falls back to software.
4. **Layout changes on a non-decode queue.** The copy moves the decoder's image `VIDEO_DECODE_DPB ->
   TRANSFER_SRC -> VIDEO_DECODE_DPB` on a compute / transfer queue. FFmpeg's own `hwdownload` does the same, and
   NVIDIA accepts it; it is the first thing to check under validation (2).
5. **The patches must be carried.** `0001` + `0005` + `0006`, 7 files against a moving GPUI. The renderer file
   (`wgpu_renderer.rs`) is the one that will conflict on rebases.
6. **A present costs what GPUI's renderer costs.** Every picture is one `renderer.draw` of the retained scene:
   instance buffers rewritten, bind groups, a render pass, about 280 heap allocations inside wgpu and GPUI on
   the main thread (`malloc-count`, §0.8), 0.2–0.25 ms of CPU. It is where most of a stream's CPU goes (the
   main thread's share in HANDOFF §4). A fast path in the renderer for "same scene, new surface pictures"
   (reuse the previous frame's buffers and bind groups, re-record only the pass) would cut it; not attempted.
7. **Software HEVC / AV1 at 1440p120** does not keep up on 4 slice / tile threads (§11). Frame threading would
   (at the cost of one frame of latency per thread); decide when the fallback matters.
8. **10-bit streams** (P010) are refused by both paths (the stream then shows nothing and reports decoder
   errors). Same design with `R16Unorm` / `Rg16Unorm` slots. The streamer sends 8-bit today.
9. **Pipelining.** The decoder waits for each picture's copy before decoding the next. At 2160p120 that is
   3.6 ms of an 8.3 ms budget on this GPU; on a much slower GPU the wait would become the limit, and the copy
   would have to be pipelined (publish from a small completion queue instead of waiting inline).
10. **Downscaling beyond 8x.** The minification filter averages at most an 8 x 8 texel box per pixel
    (`0005`). A 3840-wide picture in a view narrower than 480 px is under-sampled beyond that (mild aliasing,
    not the shimmer of plain bilinear). A mip chain would fix it at a cost per decoded frame; not needed for the
    sizes the client uses (the smallest view is the 320 px mini-player of a 1440p stream: 8x).
11. **Windows** compiles in CI only (not run yet with this crate) and draws nothing (§8). **The GL backend**
    (no Vulkan) gets `WgpuUploader`, which runs on Vulkan under `DECIBELL_VIDEO_UPLOAD=wgpu` but has never run
    on OpenGL (NVIDIA's EGL offers no surface format on this machine).
12. **FFmpeg and the driver allocate per frame** (§0.3, §0.8): about 38 `malloc` calls per decoded frame on
    the decoder thread on the hardware path, 16 on the software path, none of them ours. Reducing them means
    patching FFmpeg (its `AVBufferRef` per packet and per frame, the hwaccel's per-picture state).
13. **Thread names are cut to 15 characters by Linux**: every decoder thread shows as `decibell-video-`.

## 13. Building and running

```sh
cd gpui-client
tools/build-ffmpeg.sh                    # once: vendor/ffmpeg (about 3 minutes with 8 jobs); a no-op afterwards
tools/fetch-gpui.sh                      # vendor/gpui-src with patches 0001-0005 applied
tools/streamgen/make-streams.sh          # once: vendor/streams (needs NVENC; about 600 MB, 2 minutes)
cargo build --release -p decibell-app
cargo test -p decibell-video             # software decode through the hub; skips what needs the streams
target/release/decibell --selftest video-hw,video-idle,video-two,video-sw,video-recover,video-switch
target/release/decibell --screen video-test --dbv vendor/streams/1440p120_hevc.dbv,vendor/streams/1080p60_av1.dbv --mirror
tools/streamgen/measure.sh <label> 30 vendor/streams/1440p120_hevc.dbv      # one measured run (HANDOFF §4)
```

Environment: §0.9. The prototype this came from is still under `vendor/scratch/` on the machine it was built
on (`video-proto/`, its tools, its results in `shots/`); nothing in the repository depends on it.

## 14. Files

| Path | What |
|---|---|
| `gpui-client/docs/video-design.md` | this document |
| `gpui-client/crates/decibell-video/` | the crate (§0.1) |
| `gpui-client/patches/0001-gpui-external-surfaces.patch`, `0005-gpui-surface-fit-filter-device.patch`, `patches/README.md` | the GPUI side |
| `gpui-client/tools/build-ffmpeg.sh` | builds `vendor/ffmpeg` (§9) |
| `gpui-client/tools/streamgen/streamgen.c`, `build.sh`, `make-streams.sh`, `dbvcat.py` | the `.dbv` generator (the Linux twin of the Qt spike's; same format), the standard test set, the resolution-switching stream |
| `gpui-client/tools/streamgen/measure.sh` | one measured playback run on top of `tools/perf/measure.sh` |
| `gpui-client/tools/streamgen/malloc-count.c` | `LD_PRELOAD` allocation counter per thread |
| `gpui-client/vendor/{ffmpeg, ffmpeg-build, streams, bin}` | gitignored: the prefix, its build tree and downloads, the test streams, the built tools |
| `gpui-client/vendor/scratch/` | gitignored: the prototype and its evidence (`shots/matrix2.txt` is §11's table) |
