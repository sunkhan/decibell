# Watching a stream in the GPUI client: design and prototype results

Status: prototype built and measured on Linux / NVIDIA, 2026-11-26. GPUI pin: Zed `a3ce8d5e` (2026-11-26),
wgpu 30.0.0 at `cddb2f5d` (`Cargo.toml:1017-1018`). Paths starting with `crates/` are in the pin
(`gpui-client/vendor/zed`), line numbers are the unpatched pin's. wgpu paths are in the pinned wgpu checkout
(`~/.cargo/git/checkouts/wgpu-*/cddb2f5`), FFmpeg paths in FFmpeg 8.1.3.

Contents: [1 Decision](#1-decision) · [2 What GPUI and wgpu give us](#2-what-gpui-and-wgpu-give-us-answers-from-source) ·
[3 The Linux options](#3-the-linux-options) · [4 The GPUI patch](#4-the-gpui-patch) ·
[5 Threads, queues, sync](#5-threads-queues-and-synchronisation) · [6 Frame pacing](#6-frame-pacing) ·
[7 Several streams, two places, changes, device loss](#7-several-streams-one-stream-in-two-places-changes-device-loss) ·
[8 Windows and macOS](#8-windows-and-macos) · [9 FFmpeg build](#9-the-ffmpeg-build) ·
[10 Vendor matrix](#10-vendor-matrix) · [11 Measurements](#11-measurements) · [12 Open risks](#12-open-risks-and-what-is-not-proven) ·
[13 Rebuilding the prototype](#13-rebuilding-the-prototype) · [14 Files](#14-files)

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
files, +574 / -46 lines, of which 150 are the new `wgpu_surface.rs`; per-file table in `patches/README.md`. What it adds:

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
- A slot is free when the decoder holds the only reference to its token. The token is cloned into
  `WgpuSurfaceFrame::in_use`, so a slot stays taken while it is the newest picture and while any submitted
  frame still samples it. Five covers: newest + up to two in flight + one to write, with one spare. If none
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

1. The decoder thread calls `PresentSignal::notify()`: an atomic flag plus one message on a channel a
   foreground task waits on. If the flag is already set (another picture, or another stream, since the main
   thread last looked) nothing is sent: at most one wake per turn of the main loop.
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
  stream must set it to `None` (the prototype does), or to `None` only while a stream is visible.
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
  until the first new one. Not exercised.
- **A stream ends**: the decoder thread exits; the surface keeps its last picture until the view drops it or
  calls `clear()`. No further presents happen (§11: zero draws after the stream stopped).
- **Device loss**: GPUI detects it and rebuilds its context on the next frame (`WgpuRenderer::recover`,
  `wgpu_renderer.rs:2174`; all windows adopt the new device). Everything of ours on the old device is dead with
  it. The video layer must: notice (any Vulkan call or FFmpeg call failing with device-lost, or
  `shared_context().device` no longer being the device the `VideoDevice` was made from), drop every decoder
  and the `VideoDevice`, build a new `VideoDevice` from the new context (the hook runs again on its own), reopen
  the decoders and ask each streamer for a keyframe. `WgpuSurface`s survive (they hold only frames; call
  `clear()`). Designed, not built. A real device loss was seen once in the prototype (§12).

## 8. Windows and macOS

**Windows (design; must compile later, not built here).** gpui_windows renders with Direct3D 11: one
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

A second prefix, **`gpui-client/vendor/ffmpeg`**, built by `gpui-client/vendor/scratch/tools/build-ffmpeg-gpui.sh`
(a variant of `electron-client/native/scripts/build-ffmpeg-linux.sh`; the Electron script and prefix are
untouched). FFmpeg **8.1.3**, static, LGPL (no `--enable-gpl`, no `--enable-nonfree`), linked through
`ffmpeg-sys-next` 8.1 with the `static` feature and `PKG_CONFIG_PATH=gpui-client/vendor/ffmpeg/lib/pkgconfig`.

- **Why static, why a second prefix**: static keeps one self-contained binary and the exact FFmpeg the Vulkan
  code was tested with (the hwaccel is young: distributions ship 7.x to 9.x). The Electron addon's prefix has
  encoders only and is built into a `.node` that must not grow; this one is a superset (same NVENC / VAAPI
  encoders, so the GPUI client can stream from the same libraries) plus decoders.
- **Why 8.1.3 and not the system's 9.1**: `ffmpeg-next` / `ffmpeg-sys-next` 8.1 are what the workspace already
  uses and they bind 8.0 / 8.1. The system FFmpeg 9.1 decodes all three codecs with `-hwaccel vulkan` on this
  driver too (checked with the CLI before building anything).
- **The script fetches and builds, with no sudo**: nasm 2.16.03 (not installed here; without it the software
  decoders lose their SIMD), Vulkan-Headers v1.4.335 (header-only; not installed here; FFmpeg `dlopen`s
  `libvulkan.so.1` at run time), nv-codec-headers n13.0.19.0 (as Electron), meson in a venv and **dav1d 1.5.3
  as a static library** (BSD-2-Clause). Needs from the system: a C toolchain, git, curl, python3, ninja, libva
  and libdrm headers.

```
./configure --prefix=$PREFIX --pkg-config-flags=--static --extra-cflags=-I$PREFIX/include --extra-ldflags=-L$PREFIX/lib \
  --enable-static --disable-shared --enable-pic \
  --disable-programs --disable-doc --disable-network --disable-autodetect --disable-everything \
  --enable-avcodec --enable-avutil --enable-swscale --enable-avformat --enable-swresample \
  --enable-vulkan --enable-ffnvcodec --enable-nvenc --enable-nvdec --enable-vaapi --enable-libdrm --enable-libdav1d \
  --enable-decoder=h264,hevc,av1,libdav1d \
  --enable-parser=h264,hevc,av1 \
  --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,av1_frame_split,av1_frame_merge,extract_extradata \
  --enable-hwaccel=h264_vulkan,hevc_vulkan,av1_vulkan \
  --enable-hwaccel=h264_nvdec,hevc_nvdec,av1_nvdec \
  --enable-hwaccel=h264_vaapi,hevc_vaapi,av1_vaapi \
  --enable-encoder=h264_nvenc,hevc_nvenc,av1_nvenc --enable-encoder=h264_vaapi,hevc_vaapi,av1_vaapi \
  --enable-filter=scale --enable-protocol=file
```

- `av1` is FFmpeg's native AV1 decoder, which only decodes through a hwaccel; `libdav1d` is the software one
  (`avcodec_find_decoder(AV_CODEC_ID_AV1)` returns dav1d, so the hardware path opens `"av1"` by name).
- The prefix is 8 static libraries; the prototype binary (GPUI + wgpu + FFmpeg + dav1d, release, with limited
  debug info) is 34.6 MB stripped (316 MB with its debug info). It loads `libva.so.2`, `libva-drm.so.2` and `libdrm.so.2`
  dynamically (FFmpeg's VAAPI support, as in the Electron prefix): present on every desktop distribution, but a
  hard dependency of the binary.
- Everything that touches FFmpeg's Vulkan structs (`AVVulkanDeviceContext`, `AVVkFrame`) is in a 169-line C shim
  compiled against the prefix's headers (`video-proto/src/shim.c`), so struct layouts come from the real
  headers of whatever FFmpeg version is linked.
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

1. **One device loss, cause not established.** In the first measurement series (before the device-wide lock
   of §5.3 existed) one run out of about 95 starts lost the Vulkan device at its second decoded frame: kernel
   log `NVRM: Xid 32 ... name=video-proto` ("invalid or corrupted push buffer stream"), then
   `VK_ERROR_DEVICE_LOST` from our fence wait and from FFmpeg's next submit; GPUI stopped drawing as well. The
   likeliest cause found by reading: wgpu calls `vkDeviceWaitIdle` when it reconfigures the swapchain (which
   happens at window start-up) while the decoder thread was submitting, which Vulkan forbids; the lock removes
   that. It is not proven to be the cause: 300 forced reconfigures under load *without* the lock did not
   reproduce it. After the lock: 0 failures in 250 soak starts (150 split-mode, 100 shared-mode with two streams) and
   about 40 measurement runs, no Xid in the kernel log. With a base rate near 1 in 100 that is encouraging, not
   conclusive. Until a long soak on several machines says otherwise, treat a device
   loss as something that happens, and build the recovery of §7 before shipping.
2. **No validation layers were available** (`vulkan-validation-layers` is not installed and nothing was
   installed system-wide), so the raw Vulkan in the copy path and the FFmpeg-on-foreign-device setup have only
   been checked against the spec by reading, and against one driver by running. First thing to do on a machine
   with the layers: run the prototype with `VK_INSTANCE_LAYERS=VK_LAYER_KHRONOS_validation`.
3. **AMD and Intel are unproven.** Specifically unknown there: whether RADV / ANV accept `vkCmdCopyImage` from a
   decode-profile image on the general queue (the spec allows it when the image has `TRANSFER_SRC` usage, which
   FFmpeg sets, `vulkan_decode.c:1221`), and the shared-queue mode's behaviour on a real single-queue device.
4. **Layout changes on a non-decode queue.** The copy moves the decoder's image `VIDEO_DECODE_DPB ->
   TRANSFER_SRC -> VIDEO_DECODE_DPB` on a compute / transfer queue. FFmpeg's own `hwdownload` does the same, and
   NVIDIA accepts it; it is the first thing to check under validation (2).
5. **The patch must be carried.** 7 files against a moving GPUI. The renderer file (`wgpu_renderer.rs`) is the
   one that will conflict on rebases; the dead surface pipeline upstream suggests they intend to fill it in, at
   which point the patch shrinks to the hook and the lock, or upstream grows its own.
6. **Downscaling quality.** Bilinear sampling of a 1440p picture into a tile smaller than half its size
   aliases (fine text shimmers). Fix inside the same shader when the stream grid is built: a 4-tap box when the
   quad is under half the picture's size, or mipmaps generated on the aux queue for small tiles only.
7. **Software HEVC / AV1 at 1440p120** does not keep up on 4 slice / tile threads (§11). Frame threading would
   (at the cost of one frame of latency per thread); decide when the fallback matters.
8. **10-bit streams** (P010) are refused by the prototype. Same path with `R16Unorm` / `Rg16Unorm` slots
   (`Features::TEXTURE_FORMAT_16BIT_NORM`). The streamer sends 8-bit today.
9. **Pipelining.** The decoder waits for each picture's copy before decoding the next. At 2160p120 that is
   3.6 ms of an 8.3 ms budget on this GPU; on a much slower GPU the wait would become the limit, and the copy
   would have to be pipelined (publish from a small completion queue instead of waiting inline).
10. **Paused / hidden streams still decode and copy.** The real client should stop copying (and ideally
    decoding, via the existing stop-watching request) when no element painted the surface recently;
    `WgpuSurface::draws()` is the signal.
11. **X11, the GL backend, Windows compile**: the patch compiles for Linux only here. X11 uses the same
    renderer (`Fifo` present mode) and was run once under XWayland (1440p120 HEVC: 720 of 722 pictures drawn in 6 s); the GL backend (no Vulkan) gets the software path by
    construction but was not run; `gpui_windows` was not compiled with the patched `PaintSurface` (it never
    constructs one, and its `draw_surfaces` ignores the payload, so it should).

## 13. Rebuilding the prototype

Everything below lives under `gpui-client/vendor/scratch/` (gitignored) except the patch and this document.

```sh
cd gpui-client/vendor/scratch
# 1. FFmpeg prefix -> gpui-client/vendor/ffmpeg   (about 3 minutes with 8 jobs)
JOBS=8 ./tools/build-ffmpeg-gpui.sh
# 2. Patched GPUI: a clone of the pin with the patch applied
git clone --depth 1 file://$PWD/../zed zed-patched
git -C zed-patched apply ../../patches/0001-gpui-external-surfaces.patch
# 3. Test streams (system FFmpeg with NVENC; any ffmpeg dev package works)
cc -O2 tools/streamgen/streamgen.c -o tools/streamgen/streamgen $(pkg-config --cflags --libs libavcodec libavutil) -lm
./tools/streamgen/streamgen streams/1440p120_av1.dbv av1_nvenc 2560 1440 120 20000 30    # <out> <enc> <w> <h> <fps> <kbit/s> <s>
# 4. The prototype
cd video-proto
export CARGO_TARGET_DIR=$PWD/../target-video CARGO_BUILD_JOBS=8 PKG_CONFIG_PATH=$PWD/../../ffmpeg/lib/pkgconfig
cargo build --release
# 5. Run
../target-video/release/video-proto ../streams/1440p120_hevc.dbv ../streams/1440p120_av1.dbv
#    --software          software decode          --mirror        stream 0 also in a floating mini-player
#    --seconds N         quit after N seconds     --stop-after S  streams end after S seconds (draws must stop)
#    --resize-test       resize the window 25 times a second      --size WxH     window size
#    DBV_SHARED_QUEUE=1  decode and copy on the renderer's own queue (the AMD / Intel mode)
#    DBV_FFMPEG_VERBOSE=1  FFmpeg log at verbose
# 6. Measure
../tools/run-measure.sh <label> 20 ../streams/1440p120_hevc.dbv      # prints CPU, memory, wakeups per thread, GPU
../tools/stress.sh 150 2 ../streams/1440p120_hevc.dbv                # 150 starts of 2 s, counts device losses
```

`video-proto/Cargo.toml` depends on the patched GPUI by path, repeats the three `[patch.crates-io]` entries of
Zed's workspace that GPUI needs (async-task, async-process, calloop), pins wgpu to GPUI's revision, and starts
from Zed's `Cargo.lock` so every dependency is the version the pin was tested with.

## 14. Files

In the repository:

- `gpui-client/docs/video-design.md`: this document.
- `gpui-client/patches/0001-gpui-external-surfaces.patch`, `gpui-client/patches/README.md`.

In `gpui-client/vendor/` (gitignored; lift these into the real crate):

| Path | What | Reuse |
|---|---|---|
| `ffmpeg/` | the FFmpeg prefix (built) | as is |
| `scratch/tools/build-ffmpeg-gpui.sh` | builds it | move to `gpui-client/scripts/` |
| `scratch/tools/streamgen/streamgen.c` | `.dbv` generator (Linux twin of the Qt spike's) | test tool |
| `scratch/tools/dbvcat.py` | joins `.dbv` files into a resolution-switching stream | test tool |
| `scratch/tools/measure.py`, `run-measure.sh`, `matrix-table.py`, `stress.sh` | measurement and soak harness (the tables of §11 are `matrix-table.py shots/matrix2.txt`) | the Linux half of the perf tools |
| `scratch/video-proto/src/vkvideo.rs` | **the decoder module**: device hook, `VideoDevice`, `VulkanStreamDecoder` (decode, copy, slots) | lift nearly as is |
| `scratch/video-proto/src/swvideo.rs` | software decoder + upload | lift |
| `scratch/video-proto/src/shim.c`, `build.rs` | the C shim over FFmpeg's Vulkan structs | lift |
| `scratch/video-proto/src/dbv.rs` | `.dbv` reader (DBV1 and DBV2) | test tool |
| `scratch/video-proto/src/main.rs` | the GPUI app: present signal, element, feeder threads, counters | reference for the stream view |
| `scratch/zed-patched/` | the pin with the patch applied (`git diff` there is the patch) | regenerate from the patch |
| `scratch/streams/*.dbv` | 1080p60, 1440p120, 2160p60, 2160p120 x H.264 / HEVC / AV1, and `switch_hevc.dbv` | test data |
| `scratch/shots/` | `matrix1.txt` (first series), `matrix2.txt` (final), per-run logs, soak results, `hevc_window.png`, `mirror_window.png` | evidence |
| `scratch/target-video/` | cargo target directory of the prototype (2.3 GB; delete freely) | - |
| `scratch/ffmpeg-build/` | FFmpeg, dav1d, nasm sources and build trees (delete freely; the prefix is `vendor/ffmpeg`) | - |
