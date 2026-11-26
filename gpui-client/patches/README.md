# Patches against the pinned GPUI

The GPUI client builds against Zed's `gpui*` crates (Apache-2.0) at a pinned commit, with the patches in this
folder applied in lexical order. Pin: `a3ce8d5e88e35697b2ba1cf9cb3c6816089b8281` (2026-11-26), in
`gpui-client/GPUI_PIN`. Every patch touches Apache-2.0 crates only.

| Patch | What it does | Why |
|---|---|---|
| `0001-gpui-external-surfaces.patch` | Lets an element paint a GPU image the application owns (a decoded video picture) as a scene primitive, drawn by gpui_wgpu; lets the application reach the renderer's wgpu device and add to how its Vulkan device is created; adds a present-without-repaint request. | Watching a stream with hardware decode and no CPU copy. Design, measurements and the reasoning: `gpui-client/docs/video-design.md` (§4 is this patch). |
| `0002-gpui-frame-counters.patch` | `Window::frames_drawn()` / `frames_presented()` and the process-wide atomics `gpui::FRAMES_DRAWN` / `FRAMES_PRESENTED`. | The client's frame instrumentation and self-tests (`decibell_harness::perf`): GPUI has no hook that says a frame was drawn or presented. |
| `0003-wgpu-vulkan-before-gl.patch` | The renderer tries a Vulkan-only wgpu instance first and creates the Vulkan + OpenGL one only if no discrete or integrated Vulkan GPU can configure the window's surface. | Creating the OpenGL backend initialises EGL, which loads every EGL vendor library (Mesa with LLVM, NVIDIA's GL core) although Vulkan then draws everything: −20 MB private memory, −52 MB RSS, −30 ms startup here. |
| `0004-x11-park-refresh-timer.patch` | On X11 the per-window refresh timer stops after a tick with nothing to draw and restarts from `PlatformWindow::schedule_frame`. | The X11 backend woke the main thread at the monitor's refresh rate for as long as a window was visible (280 times a second here), drawing nothing. |
| `0005-gpui-surface-fit-filter-device.patch` | On top of `0001`: a surface is fitted (contain / cover / fill) by the renderer from the picture it draws, minified pictures are box-filtered, a picture names its device (and is skipped by a renderer on another), a draw observer reports each picture's first frame, a device loss can be simulated. | What the real stream view (`crates/decibell-video`) needed beyond the prototype: object-fit that survives a resolution change without a repaint, small tiles that do not shimmer, recovery from a lost GPU device, per-picture "presented" times. |
| `0006-wgpu-keep-lost-instance.patch` | After a device loss the renderer no longer destroys the lost context's wgpu instance. | With two windows open, the second window to recover crashed inside NVIDIA's driver (segfault in `vkGetPhysicalDeviceSurfaceSupportKHR`). Found by the `video-recover` self-test. |
| `0007-wgpu-retained-frames.patch` | The wgpu renderer keeps what it built on the GPU for the scene it drew last (instance data, bind groups, the list of draw calls) and encodes a frame of the same scene from it, with the pictures the scene's external surfaces hold by then; `Scene::version()` says "the same scene"; counters and a frame capture for testing it. | Every video picture is a present of an unchanged scene (`Window::request_present`), and the renderer wrote the whole scene to the GPU again for each one: half of the UI thread's allocations per picture and a quarter of its time in `draw`. |
| `0010-gpui-text-engine.patch` | What `decibell-text` needs from GPUI's text system: shaping without GPUI's per-window cache (`TextSystem::shape_line_uncached`), a glyph's unkerned advance, a font's metrics and its glyph for a character; and on Linux, synthesised styles (an italic request on a family without an italic face is slanted by 0.25, a weight of 600 or more on a family lighter than 600 is emboldened by 1/24 em, each as a `FontId` of its own) and families the application added that have no letters (KaTeX's symbol faces) resolving by name. | The client lays text out itself (`crates/decibell-text/README.md`). Chromium synthesises exactly these styles for the renderer, which loads no italic face. |
| `0011-linux-deferred-system-fonts.patch` | On Linux the text system starts with an empty font database (`CosmicTextSystem::new_with_deferred_system_fonts`) and indexes the installed fonts only if a family is requested that no added font provides. | Indexing 1,095 installed faces and walking that index for every new (family, weight, style) cost about 85 ms of the time to the first frame; the client names only its bundled families, and `decibell-text` adds a fallback font when a character needs one. |
| `0012-gpui-physical-key-codes.patch` | Key events carry the physical key: `KeyDownEvent::code` / `KeyUpEvent::code` (`KeyCode::Evdev(n)` on Linux, `KeyCode::Windows(scan)` on Windows, `Unknown` elsewhere and for simulated keystrokes), and `ModifiersChangedEvent::key` names the modifier key that went down or up. | Hotkey bindings name keys by position (`KeyboardEvent.code`); GPUI's `Keystroke` has only the character the layout types, so a recorder built on it saved the wrong key on every non-US keyboard (`HANDOFF.md` §5, now closed). |
| `0013-gpui-hide-cursor-until-mouse-moves.patch` | `App::hide_cursor_until_mouse_moves()`: the public door to what the pinned GPUI already does on every platform when a key is typed (the cursor hidden until the pointer next moves). | A stream shown full screen hides the cursor once the pointer has rested, as the renderer's `cursor-none` does; GPUI has no hidden `CursorStyle`, and it does not need one. |
| `0014-gpui-replay-nested-cached-views.patch` | A clean cached view nested in a cached view that renders is replayed from its own cache instead of rendering with it: views record which frame their replay ranges index, a replayed range moves the ranges of the views inside it, the cache key gains the rem size; counters (`gpui::CACHED_VIEWS_RENDERED` / `_REPLAYED`) and a switch back (`gpui::set_nested_view_replay`). | At the pin every notify renders every cached view under the topmost dirty one, which in this client is the whole window (the content is one cached view): a speaking ring on a 12-tile stage rendered 18 views (5 now), a typed key the message list above the field. Measured below. |
| `0015-linux-clipboard-images.patch` | Linux clipboard writes carry pictures: Wayland offers each image entry under its own type (`image/png`, …) and text types only when there is text, and serves whichever type is asked for, writing large pieces without blocking; X11 owns the selection with every entry, under its type, and hands contents larger than 1 MiB out in `INCR` pieces (also to a clipboard manager taking them at quit); a window with the keyboard focus but no press yet may take the Wayland selection (the focus event's serial, as Qt does). | "Copy image" on Linux (owner request, 2026-11-26): GPUI only ever offered text on Wayland and X11. |

## How the patches are applied

`gpui-client/tools/fetch-gpui.sh` builds `gpui-client/vendor/gpui-src`, the copy cargo compiles GPUI from
(`[workspace.dependencies]` in `gpui-client/Cargo.toml`):

1. `vendor/zed` is the pristine clone at the pin; if it is missing, the script fetches exactly that commit
   (shallow). It is never edited.
2. `vendor/gpui-src` is a git worktree of it at the pin, with every `patches/*.patch` applied by
   `git apply --index`, in lexical order.
3. `vendor/gpui-src.stamp` records the pin and each patch's name and hash. A second run is a no-op; a changed
   pin or patch rebuilds the copy from scratch. If the copy holds edits that are in no patch, the script stops
   and says so (`--force` discards them).

Run it once after cloning and whenever the pin or a patch changes. It works under Git Bash on Windows (the
Windows Native Check workflow runs it).

## Writing or changing a patch

Each patch is a diff against the pin **with the lower-numbered patches applied**. Because the applied patches
sit in the worktree's index, `git diff` there shows only what you changed since:

```sh
cd gpui-client/vendor/gpui-src
# … edit GPUI …
git add -N .                                   # so new files are part of the diff
git diff > ../../patches/0005-what-it-does.patch
cd ../.. && tools/fetch-gpui.sh --force        # rebuild the copy from the patches; must apply cleanly
```

- Number in steps, name by effect, keep one concern per patch and the diff minimal: every hunk is a conflict
  waiting at the next pin.
- To change an existing patch `N`: make a scratch worktree of `vendor/zed` at the pin, `git apply --index`
  the patches below `N`, then `git apply` (no `--index`) patch `N`, edit, and `git diff > …/N-name.patch`.
  Changing a patch can move the context later patches rely on: run `fetch-gpui.sh --force` afterwards and
  regenerate the ones that no longer apply.
- Add a row to the table above and a section below: what, why, the measured effect (before / after, with the
  command), whether it could go upstream.
- Measure with `tools/perf/` on a release build (`gpui-client/HANDOFF.md` §2). Compare binaries that live on
  the same filesystem: file-backed pages of a binary on tmpfs are accounted differently (8 MB of "private"
  moved between clean and dirty in our first run).
- On a new pin: update `GPUI_PIN`, run the script, and for each patch that fails `git apply --3way` it in the
  worktree, fix, regenerate. `0001` conflicts first (`wgpu_renderer.rs` is the file upstream changes most).

## 0002: frame counters

`crates/gpui/src/window.rs`, +27. Two `u64` fields on `Window` bumped in `Window::draw` and `Window::present`,
their getters, and two `static AtomicU64` with the same counts summed over all windows (an atomic, so that the
client's `SIGUSR1` handler can read them). "Drawn" is a frame whose element tree was rebuilt and painted;
"presented" is a scene handed to the platform window, which also happens without a redraw (scene animations,
`request_present`, the presents GPUI adds after fast input). No measurable cost: two increments per frame.
Upstream: Zed has its own `profiler` feature for this; a plain counter might be accepted but is not needed there.

## 0003: Vulkan before OpenGL

`crates/gpui_wgpu/src/wgpu_context.rs` +56 / −3, `wgpu_renderer.rs` +70 / −36.

GPUI created its wgpu instance with `Backends::VULKAN | Backends::GL`, enumerated the adapters of both and
picked by rank (the compositor's GPU, then discrete > integrated > other (OpenGL) > virtual > CPU, then Vulkan
before OpenGL), testing each by configuring the surface. The patch moves instance + surface + context
creation into one function (`create_context_and_surface`) that does this twice at most:

1. a **Vulkan-only instance**, accepting only discrete and integrated GPUs: exactly the adapters that rank
   above every OpenGL adapter, so a success is the adapter the full search would have chosen;
2. if that finds nothing (no Vulkan loader, no hardware Vulkan device, or none that can configure the
   surface), the original search over Vulkan and OpenGL with every adapter type.

Both callers use it: the first window (`WgpuRenderer::new`) and recovery after a lost device (`recover`).
The headless renderer used by captures and tests is unchanged.

Measured on the skeleton (release, gallery screen, window open and unfocused, 20 s settle, 60 s window,
`tools/perf/measure.sh`; startup: median of 5 warm runs, `tools/perf/startup.sh`; RTX 4080, NVIDIA 620.73,
KDE Plasma 6 Wayland; `0001` + `0002` applied in both columns):

| | Wayland before | Wayland after | XWayland before | XWayland after |
|---|---|---|---|---|
| Private memory (clean + dirty) | 98.0 MB | **77.6 MB** | 102.2 MB | **82.0 MB** |
| Private dirty | 38.9 MB | 28.8 MB | 39.3 MB | 29.4 MB |
| PSS | 105.2 MB | 81.6 MB | 109.4 MB | 85.9 MB |
| RSS | 167.1 MB | 114.9 MB | 171.2 MB | 119.0 MB |
| GPU memory (driver's per-process figure) | 61 MB | 57 MB | 62 MB | 58 MB |
| First frame presented, from launch | 255 ms (251–265) | **225 ms** (220–238) | 293 ms (272–295) | 285 ms (273–297) |
| EGL / Mesa / LLVM libraries mapped | 6 | 0 | 6 | 0 |

(The XWayland startup runs were taken while other builds ran on the machine; an earlier pair of runs gave
274 → 224 ms.) Not exercised here: the fallback pass on a machine without usable Vulkan. With Vulkan hidden
(`VK_LOADER_DRIVERS_SELECT=nonexistent DECIBELL_LOG=info`) the log shows the second pass ("No Vulkan GPU can
drive this window …; trying OpenGL as well", then the OpenGL adapter being tested) and the app ends as the
unpatched one does on this box, with "No GPU adapter found that can configure the display surface": NVIDIA's
EGL offers no surface format on Wayland or XWayland (`docs/gpui-notes.md` §3.6). So the fallback is reached,
but a window drawn by OpenGL has not been seen. Upstream: yes, as it stands; it changes no choice, only when OpenGL is
loaded.

## 0004: X11 refresh timer parks when idle

`crates/gpui_linux/src/linux/x11/client.rs` +53 / −6, `x11/window.rs` +25.

The X11 client ran a calloop timer per visible window at the display's refresh interval and called the
window's frame request on every tick, whether or not anything was dirty. GPUI's core already tells
demand-driven platforms when it wants a frame (`PlatformWindow::schedule_frame`, which the Wayland backend
implements and the X11 one left as the default no-op). The patch:

- gives the X11 window a `frame_demand` flag, set by `schedule_frame` and by `draw` (a presented frame can be
  followed by another without a new request: presenting after fast input, recovery from a lost device);
- lets the timer take the flag after each tick and its event processing; a tick that finds it unset ends the
  timer (`RefreshState::Parked`);
- restarts a parked timer from `schedule_frame`, through a foreground task because the client can be
  borrowed when the core asks (`X11ClientStatePtr::resume_refresh_loop`). The restarted timer fires at once,
  so the first frame after idle no longer waits for the next tick.

Expose events present directly, as before; hidden windows have no timer, as before.

Measured as above, under XWayland on a 279.87 Hz display (`0001`–`0003` applied in both columns):

| | before | after |
|---|---|---|
| Main-thread wakeups at idle | 279.9 / s (every one of 60 samples) | **0.0 / s** (60 of 60 samples at 0) |
| CPU at idle | 0.37–0.45 % of one core | 0.005 % |
| Frames drawn at idle | 0 | 0 |
| First frame presented, from launch | 224 ms (212–229) | 210 ms (204–220) |

The `idle-frames` self-test fails on X11 without this patch and passes with it (1 wakeup in 5 s: its own
timer). Upstream: yes; it is how the Wayland backend already behaves.

## 0005: surface fit, minification filter, device identity

`crates/gpui_wgpu/src/wgpu_surface.rs` +72 / −12, `wgpu_renderer.rs` +65 / −9, `shaders.wgsl` +25 / −7,
`wgpu_context.rs` +13. Only files `0001` already touches (and only gpui_wgpu: nothing changes for Windows or
macOS). `0001` is unchanged; this is a diff against the pin with `0001`–`0004` applied.

- **Fit at draw time.** `WgpuSurfaceView { surface, fit: SurfaceFit::{Fill, Contain, Cover} }` is what an element
  wraps in the `ExternalSurface` (a bare `WgpuSurface` still works and means `Fill`). The renderer computes the
  fitted rectangle (contain: centred, snapped to whole pixels) or the texture crop (cover) from the size of the
  picture it is about to draw. The element is painted once; a stream that changes resolution or aspect keeps
  its fit with no layout or paint. Corner radii round the painted bounds (the element's box, as CSS does for
  `object-fit`), so `SurfaceParams` gains `clip_bounds` and `uv_rect` (112 → 144 bytes).
- **Minification filter.** The fragment shader averages a grid of bilinear taps over each output pixel's
  footprint in the picture: `ceil(texels per pixel / 2)` taps per axis, at most 4 × 4 (an 8 × 8 texel box). At
  full size and above that is one tap, the shader of `0001`; a 2560 × 1440 picture in a 384 × 216 mini-player
  takes 4 × 4. No mip chain, no extra texture, nothing per decoded frame: the cost is paid only by the pixels of
  a small view. Measured: `gpui-client/HANDOFF.md` §4 "Stream playback" (the `--mirror` row).
- **A picture names its device** (`WgpuSurfaceFrame::device`), and `WgpuSurface::acquire` returns nothing for a
  picture of another device. After a device loss the renderer's new device never binds a texture of the old one
  (a wgpu validation panic otherwise), whatever order the decoder threads and the renderer notice the loss in.
- **`WgpuSurface::set_frame(frame, tag)` + `set_draw_observer`**: the observer is called with the tag of each
  picture the first time a rendered frame draws it (the video crate's "presented" count, interval histogram and
  trace). `picture_size()` reads the newest picture's size.
- **One `Arc<WgpuSurfaceFrame>` per slot, reused**: `set_frame` only clones the Arc and a slot is free when its
  owner holds the only reference, so publishing a picture allocates nothing (`in_use` stays for owners that make
  a frame per picture).
- **`WgpuContext::simulate_device_loss()`**: marks the device lost without the driver saying so; the renderer
  then recreates its context on the next frame exactly as after a real loss. The `video-recover` self-test uses
  it.

Checks: `decibell --selftest video-hw,video-idle,video-two,video-sw,video-recover,video-switch` (each needs
`0005`), on Wayland and XWayland. Upstream: the fit and the filter belong with `0001`'s surface drawing; the
device identity is needed by anything that hands wgpu textures across a device loss.

## 0006: the lost instance is kept

`crates/gpui_wgpu/src/wgpu_renderer.rs` +9 / −1, in `WgpuRenderer::recover`.

GPUI recovers from a lost device window by window, each at its next frame: the first creates a new instance,
adapter and device; the others find the new context and only make a new surface. The old instance lives as
long as a window still holds its old surface, so it was destroyed in the middle of the *second* window's
recovery, with the new instance already in use. On NVIDIA 620.73 (Wayland) the new instance's next
`vkGetPhysicalDeviceSurfaceSupportKHR` then jumped through a null pointer inside `libnvidia-glcore`:

```
#0  0x0000000000000000 in ?? ()
#1  … libnvidia-glcore.so.620.73.04
#4  get_physical_device_surface_support (ash … khr/surface.rs:19)
#5  surface_capabilities (wgpu-hal/src/vulkan/swapchain/native.rs:85)
#12 surface_configuration (gpui_wgpu/src/wgpu_renderer.rs)
#14 WgpuRenderer::recover
```

The patch forgets one reference to the lost context's `wgpu::Instance`, so `vkDestroyInstance` never runs for
it: one leaked instance per device loss. Before: `decibell --selftest video-recover` (which marks the device
lost with two windows open) crashed every time, in the test's own check when the main window was made to
draw, or in whatever ran next. After: passes, and the main window draws on the new device. Whether the crash
is the driver's or wgpu's ordering was not established; a single-window recovery never showed it. Upstream:
worth reporting to Zed with the trace (any two-window Zed on NVIDIA would hit it after a device loss).

## 0007: the renderer keeps the frame it built

`crates/gpui_wgpu/src/wgpu_renderer.rs` +504 / −190, `wgpu_atlas.rs` +13 / −1, `gpui_wgpu.rs` +5 / −1;
`crates/gpui/src/scene.rs` +24 (the hook in the core crate). A diff against the pin with `0001`–`0006` applied; it
touches no file of `0010` / `0011`.

**The problem.** `Window::request_present` (`0001`) presents the scene the window already has: no layout, no paint.
But `WgpuRenderer::draw` treated that scene as new every time: seven `queue.write_buffer` calls for the
instance arrays (each a staging buffer inside wgpu and a copy at submit), a bind group per array, the globals,
the gamma parameters, the atlas bind groups, the walk over the scene's batches. For a stream that is once per
picture, 120 times a second, on the UI thread.

**What changed.** `render_frame` is two steps:

1. `build_frame` (what the old `record_frame` did before its render pass): writes the scene's uniforms and
   instance data, makes the bind groups, and lists the draw calls in `RetainedFrame::draws`
   (`RetainedDraw::Instances { pipeline, instances, texture, range, depth }` or `Surface { index, depth }`).
2. `encode_frame`: resolves the scene's surfaces to the pictures they hold now (`prepare_surfaces`, as before),
   begins the pass and issues the listed draws, submits.

Every frame runs step 2. Step 1 is skipped when the frame is of **the scene drawn last, at the same size, with
the atlas's set of textures unchanged**, and the scene has external surfaces (nothing else presents an
unchanged scene often enough to matter, so a window without video takes the old path exactly: build, then
encode). Both paths encode from the same list, which is why they cannot draw different frames.

- **"The same scene"**: `Scene::version()` (gpui), a process-wide counter stamped by `Scene::finish`, by an
  animation step (`advance_animations` moves sprites in a finished scene) and cleared by `Scene::clear`. A
  window's two scenes, and two windows' scenes, never share a version.
- **The atlas**: `WgpuAtlas::before_frame` returns a revision that changes when a texture is created or released
  (an image dropped between two presents can free one the kept bind groups name). Uploads into existing
  textures are flushed by `before_frame` every frame, as before.
- **Invalidated by**: another scene (any repaint: a hover, a theme change, another view of the window), another
  size (a resize), `set_subpixel_layout`, `update_transparency` / a pipeline rebuild, a failed frame, and a new
  renderer core (device loss). Each window has its own renderer core, so two windows keep a frame each.
- **Surface parameters** (bounds, fit, matrix: 144 bytes per surface) are written only when they differ from
  what the buffer holds for that surface: for a playing stream, on its first frame.
- `gpui_wgpu::set_frame_reuse(false)` restores "build every frame"; `frame_stats()` counts frames and
  nanoseconds per kind; `capture_next_surface_frame(rebuild)` renders the next frame of a scene with surfaces
  into an offscreen texture too, by the same code, and reads it back.

**Measured** (release build, 1440p120 HEVC, Vulkan Video, one stream; `decibell --screen video-test` with
`DECIBELL_VIDEO_FASTPATH=0` for "before"; allocations with `tools/streamgen/malloc-count.c` on the UI thread;
time is `WgpuRenderer::draw` per present from `frame_stats()`; the table in `gpui-client/HANDOFF.md` §4 has the
CPU):

| | Scene written per present (`DECIBELL_VIDEO_FASTPATH=0`: the renderer before `0007`) | Kept frame (`0007`) |
|---|---|---|
| UI-thread allocations per present (1 × 1440p120, 366 presents counted, two runs each) | 281, 288 | 140, 145 |
| `WgpuRenderer::draw` per present, mean of a 9 s run, three runs each, alternating, at load 5 | 263, 237, 281 µs | 183, 188, 175 µs |
| the same on the quietest machine of the day (load 3.5, one run each, a development build) | 139 µs | 108 µs |
| UI threads' CPU, 2 × 1440p120 (HEVC + AV1), % of a core, two runs each at load 4.0–4.8 | 6.25, 6.11 | 5.04, 4.83 |
| whole process, the same runs | 11.0, 10.8 | 9.8, 9.6 |
| UI threads' CPU, 1 × 1440p120 HEVC, two runs each (load 4.0–8.5) | 3.32, 4.37 | 3.44, 3.62 |
| whole process, the same runs | 5.8, 7.7 | 6.4, 6.8 |

Two streams present twice as often, which is where the kept frame shows in CPU: the UI threads' share falls by
a fifth. With one stream the difference (about 0.8 of a point where both runs were at the same load) is inside
what the machine's load moved the same run by that day; the allocation count is not.

What is left per present is wgpu's own work for any frame, however small (acquire the swapchain texture, a
view, a command encoder, the pass, submit, present: its resource trackers are resized per command buffer, the
driver allocates in `vkQueuePresentKHR` for Wayland) and GPUI's effect flush; none of it depends on the scene.
A "cached UI texture + composite" design was the alternative: it pays exactly that fixed cost too, plus a
full-window blit per present and the bookkeeping of what is above and below each video, and saves only the
replay of the draw list, which is small for the scene measured (a few tiles, labels and discs; one draw call
per batch). It becomes worth another look if a stream view with hundreds of batches ever shows up in a profile:
that has not been measured.

**Checked by** `decibell --selftest video-fastpath`: a paused stream's window captured both ways (built from the
scene, and from the kept frame) is identical byte for byte (1280 × 720 and, after a resize, 1120 × 700: 0 bytes
differ); 240 pictures then cost 240 reused frames and none built; and under
`crates/decibell-video/tools/alloc-count.c` the UI thread stays within 170 allocations per present (138
measured). `video-two` covers resizes during playback and two surfaces of one stream, `video-recover` a device
loss (three in a row), every other self-test of the client the path without surfaces.

Upstream: the idea is general (any present of an unchanged scene), but it only pays with `0001`'s
`request_present`, and the Metal and DirectX renderers would each need their own version; ours to carry.

## 0010: what the text engine needs

`crates/gpui/src/text_system.rs` +24, `crates/gpui_wgpu/src/cosmic_text_system.rs` +109 / −3.

**`TextSystem`** (all platforms, additive):

- `shape_line_uncached(text, font_size, &[FontRun]) -> LineLayout`: the platform shaper's result as it is.
  `WindowTextSystem::layout_line` caches per window and drops what a frame did not use; `decibell-text` keeps
  its own layouts for as long as a message is cached, so GPUI's cache would only hold a second copy.
- `glyph_advance(font, glyph, size)`: the advance without kerning (a line that ends inside a shaped run is as
  wide as Chromium's re-shaped line, whose last glyph has lost its kerning).
- `font_metrics(font) -> FontMetrics` and `glyph_for_char(font, char)`: line boxes are built from the font's
  own ascent and descent, and formulas draw single glyphs by character.

**`CosmicTextSystem`** (Linux):

- **Synthesised styles.** `font_id` returns a *twin* `FontId` when the matched face lacks what was asked for:
  `oblique` when the request is italic or oblique and the face is upright, `bold` when the request is 600 or
  more and the face is lighter than 600 (CSS `font-synthesis`, the rule Blink applies to a web font). The twin
  shares the face (shaping and advances are unchanged) and differs only in rasterisation: swash's
  `Render::transform` with a skew of **0.25** (Skia's `SK_Scalar1 / 4`, what Blink sets for a synthetic italic;
  measured on a 120 px glyph in Electron 33: 0.250) and `Render::embolden(size / 24)` (FreeType's emboldening,
  which Skia uses on Linux). Because the twin is its own `FontId`, GPUI's raster-bounds cache and glyph atlas
  keep slanted and upright glyphs apart without a new key field. GPUI's own text elements get italics too.
- **Symbol families by name.** A face without an `m` glyph was never accepted as a requested font (Zed measures
  its editor with `m`). A face the application added itself (`fontdb::Source::Binary`) now is: KaTeX's
  `KaTeX_Size1…4` and `KaTeX_AMS` have no letters, and `resolve_font` panics when nothing resolves.

Windows: untouched. DirectWrite's font matching is expected to return simulated oblique and bold faces by
itself (its oblique is 20°, not Skia's 14°); unverified, see `crates/decibell-text/README.md` § Windows.
Upstream: the `TextSystem` accessors perhaps; synthetic styles are a real gap in GPUI on Linux and could go
upstream with a `FontStyle`-driven design.

## 0011: system fonts on Linux, on demand

`crates/gpui_wgpu/src/cosmic_text_system.rs` +50 / −7, `crates/gpui_linux/src/linux/platform.rs` +3 / −1.

`LinuxCommon::new` built the text system with `CosmicTextSystem::new`, which indexes every installed font
before the first frame (1,095 faces here), and cosmic-text then walks that whole index, opening most font
files, for every new (family, weight, style) it shapes. The patch adds
`CosmicTextSystem::new_with_deferred_system_fonts` (an empty database, the locale `FontSystem::new` would have
picked) and uses it; `load_family` indexes the installed fonts the first time a family is asked for that no
added font provides, logs that it did, and goes on as before. So an application that names only fonts it
added never pays, and one that names a system family behaves as it always did, one index later.

What an application then has to do itself is fallback for characters its fonts lack: with an empty database
cosmic-text has nothing to fall back to. `decibell-text` does it in its shaping call
(`crates/decibell-text/src/fallback.rs`): a glyph that came back missing is looked up with fontconfig
(`dlopen`ed then, never linked), the one font file is mapped and added with `add_fonts`, and the run is shaped
again before the layout is returned. The database holds the bundled faces plus the fonts conversations
actually needed, so font queries stay cheap for the life of the process.

Measured on the client (release, the token gallery as the first screen, KDE Plasma 6 Wayland, RTX 4080;
`0001`–`0006` + `0010` in both columns; the machine was running other builds, see the ranges):

| | before | after |
|---|---|---|
| First frame presented, from launch (`tools/perf/startup.sh --runs 6`, median and range) | 277 ms (262–387) | **191 ms** (155–221) |
| Private dirty memory, window idle for 9 s (`/proc/<pid>/smaps_rollup`, two runs each) | 31.0 / 30.8 MB | **30.1 / 30.2 MB** |
| Private memory (clean + dirty) | 62.8 / 62.2 MB | 61.4 / 61.6 MB |
| "indexing the installed fonts" in the log (`DECIBELL_LOG=info`, `--selftest boot`) | — | never |
| First text in a script the bundled fonts lack (CJK): fontconfig loaded, font found, mapped, added, re-shaped | 0 (already indexed) | 21 ms, once per process |
| A later new fallback font (Arabic; an emoji font) | 0 | 3.8 ms; 1.0 ms |
| Text in a script whose font is loaded | — | no extra cost |

The 21 ms can be moved off the frame that first needs it: `decibell_text::prewarm_system_fonts()` loads
fontconfig from any thread (the app can call it on a background thread once the first frame is out). Not
exercised: a machine without libfontconfig (no fallback: the glyph stays missing, a warning is logged).
Upstream: the constructor is generic and could go; Zed itself wants the system fonts.

## 0012: the physical key on key events

**What.** A key event says which key it is, not only what the keyboard layout makes it type.

- `gpui::KeyCode` (new, `interactive.rs`): `Unknown`, `Evdev(u16)`, `Windows(u16)`. GPUI names no keys:
  the number is the platform's, and the application maps it (the client: `decibell_model::live::hotkeys::
  code_of`, through the core's `hotkeys/keys.rs` table, to the DOM's `KeyboardEvent.code` names).
- `KeyDownEvent::code`, `KeyUpEvent::code`.
  - **Wayland**: the `key` argument of `wl_keyboard.key`, which is the evdev code (`KEY_*`). The repeated
    key-down events the client synthesises carry it too.
  - **X11**: the event's keycode minus 8 (X keycodes are evdev codes plus 8); below 8: `Unknown`.
  - **Windows**: the set-1 scan code from bits 16-23 of the key message's `lParam`, plus `0xE000` when the
    extended-key bit (24) is set: `0x001D` is the left Control, `0xE01D` the right one, `0xE048` the
    arrow up, `0x0048` the numpad 8. A message whose scan code is 0 (some injected input): `Unknown`.
  - **macOS, web**: `Unknown` (not implemented: `NSEvent.keyCode` and `KeyboardEvent.code` are there to
    take when those ports matter).
  - A keystroke that was never a key: `Window::dispatch_keystroke`, replayed pending input, the one-byte
    IME commit Wayland re-injects as a key press: `Unknown`.
- `ModifiersChangedEvent::key: Option<ModifierKey { code, pressed }>`. A modifier key is not a key event
  in GPUI: it only shows as a change of `modifiers`, which cannot tell the left Shift from the right one
  and reports nothing when a second modifier of the same kind is pressed. Now Linux and Windows send one
  `ModifiersChanged` per press and release of a modifier key with `key` set, carrying the modifiers **as
  last reported** (on Linux the new state arrives in its own event, `wl_keyboard.modifiers` /
  `XkbStateNotify`, with `key: None`, in no fixed order with the key on X11). Windows already built its
  event from the key message: there the patch only stops dropping the ones that change no modifier
  (auto-repeats are still dropped). An event for another reason (focus, a layout switch): `None`.

**Why.** Bindings are stored by physical key (`Semicolon`, `KeyQ`), as the Electron client records them
from `KeyboardEvent.code`, and the core's listeners watch evdev codes and scan codes. A recorder that
has only the character saves the wrong key on a German keyboard (Z is on the `KeyY` key) and cannot save
ş, ğ, ü, ö, ç or ß at all; and modifier-only bindings (`ControlRight`) need the side.

**Cost.** One `u32`-sized field per key event; one more `ModifiersChanged` event per modifier key press
and release (no frame: nothing listens that redraws; the `idle-frames` and kit self-tests are unchanged).

**Evidence.**
- `decibell-model`: `live::hotkeys::tests::layouts` compiles the `tr` and `de` layouts with xkbcommon,
  takes what each evdev code types under them, builds the events the Linux layer builds, and checks the
  recorder stores `Semicolon` for ş, `KeyI` / `Quote` for ı / i, `KeyY` for the German Z, sided
  modifier-only combos, and that the focused fallback fires `Control + Semicolon` on that key under `tr`,
  `de` and `us`.
- `decibell --selftest hotkeys-layout`: the codes pass through GPUI's dispatch to key-down, key-up and
  modifiers listeners; and on X11 (run with `WAYLAND_DISPLAY=`) real X key events sent to the test window
  alone (`XSendEvent`, python-xlib) arrive with keycode − 8, the right Control as a modifier key with
  `Evdev(97)`.
- Not run: the Wayland path with a real key (nothing can inject one into a single window; the code is
  `wl_keyboard.key`'s own argument), and everything on Windows (written to compile; no Windows target is
  installed here, so not even compile-checked: the Windows Native Check workflow does that).

**Files.** `gpui/src/interactive.rs` (+43: the types and fields), `gpui_linux/src/linux/wayland/client.rs`
(+21), `gpui_linux/src/linux/x11/client.rs` (+36), `gpui_windows/src/events.rs` (+44 / −6), and the
struct literals that needed the new field (`gpui`: `window.rs`, `elements/div.rs`, `key_dispatch.rs`,
`window/native_view.rs`, `app/test_context.rs`; `gpui_macos`; `gpui_web`). Zed's own crates that build
these events (`agent_ui`, `keymap_editor`, `terminal_view`, `vim`…) are not patched: they are not part of
the client's build.

**Upstream.** A physical key code on key events is something GPUI lacks for every application with
rebindable shortcuts; the shape here (a raw platform number) is the smallest one. A version for upstream
would fill macOS and the web and probably name the keys.

## 0013: hiding the cursor until the pointer moves

**What.** One public method, `App::hide_cursor_until_mouse_moves()`, calling the platform method of the
same name.

**Why not a hidden `CursorStyle`.** The brief for it assumed the pin had no way to hide the cursor. It has:
since Zed hides the cursor while typing (`CursorHideMode`, `App::set_cursor_hide_mode`), every platform
implements `Platform::hide_cursor_until_mouse_moves` and shows the cursor again on the next pointer motion:

| Platform | Hides with | Shows again |
|---|---|---|
| Wayland | `wl_pointer.set_cursor(serial, None, 0, 0)` on the window under the pointer (`gpui_linux/src/linux/wayland/client.rs`, `hide_cursor_until_mouse_moves`) | on `wl_pointer.motion` / `enter` / a button: the kept style is set again (`restore_cursor_after_hide`); a style set meanwhile is kept and not shown |
| X11 | an invisible cursor (made once) as the window's cursor attribute | on motion, enter or a button: the window's kept style |
| Windows | `SetCursor(None)` if the pointer is over one of our windows, a flag otherwise | `WM_SETCURSOR` / mouse move sets the kept cursor; leaving the window clears the flag |
| macOS | `NSCursor.setHiddenUntilMouseMoves(true)` | AppKit |

Only the trigger was private (two call sites in `window.rs`, both for keyboard input). A `CursorStyle::None`
would have needed a new arm in each platform's style table and would still have had to solve what the
existing code already solves (a style request arriving while hidden, the pointer leaving and entering).
So the patch is the door, nine lines.

**How the client uses it.** `decibell_voice::focus::rest_cursor`: when the overlays' one-shot timer fires
(2.5 s after the last pointer movement over the video, `OVERLAY_HIDE`) and the stream fills the window
(`watching.fullscreen`), the cursor is hidden. The pointer moving shows it (the platform) and the overlays
(the view's `on_mouse_move`), and re-arms the timer. No timer of its own, nothing while not fullscreen.

**Evidence.** `decibell --selftest voice-stall` (part 2): not hidden outside fullscreen; in fullscreen hidden
2.5 s after the pointer rested; not while it keeps moving; hidden again after the next rest. The test moves
GPUI's pointer, not the real one, so it asserts the call and logs what the platform reports
(`App::is_cursor_visible`: hidden when the real pointer is over the test window). **By eye**: with a real
pointer under Wayland and XWayland the owner should see the cursor go and come back
(`docs/video-owner-checks.md`). Windows and macOS: upstream's code, compiled by the Windows check, not run
here.

**Upstream.** Trivially: a public method for something public behaviour already depends on.

## 0014: clean cached views nested in a dirty one are replayed

**What the pin does.** `AnyView::cached` keeps what a view prepainted and painted and replays it while the
view is clean and its key (bounds, content mask, inherited text style, opacity) holds. But when a cached
view renders, `prepaint_view` sets `window.refreshing` for the duration, and a view checks `!refreshing`
before replaying: **every cached view inside a cached view that renders, renders too**. And a notify
marks the view's ancestors dirty with it (`Window::mark_view_dirty` walks the view path), so the topmost
cached view of the window renders on every change. In this client that is the content under the title bar
(`Root` draws it `.cached`): any change anywhere rendered every view of the window. Caching views paid
only for frames the title bar or a toast caused.

**Why the pin does it: the replay ranges go stale.** What a view replays are index ranges into the
previous frame's lists (`PrepaintStateIndex`: hitboxes, tooltips, deferred draws, dispatch-tree nodes,
accessed element states, line layouts; `PaintIndex`: scene operations, mouse listeners, input handlers,
cursor styles, tab stops, …), written into the view's element state when it is drawn or replayed. A view
inside a cached ancestor that is replayed **whole** is not visited that frame: its content is copied along
at new indices, but its own ranges still name the frame before. If the ancestor then renders and the
nested view replays, it copies whatever sits at the old indices. Forcing the subtree to render is the
blunt way never to read a stale range. (Simply not forcing it, the obvious two-line change, panics in
`key_dispatch.rs`, "node 35 was not part of the reused subtree 26..30", as soon as something drawn
*before* a replayed ancestor changed its number of hitboxes between two frames; without a focused element
it silently replays other views' hitboxes, listeners and paint. The test below does exactly that.)

**What the patch does.**

1. *A range knows its frame.* `ViewElementState` gains `prepaint_frame` / `paint_frame`: the window's
   `frames_drawn` (patch `0002`) while the range was written. A view replays only if both are the previous
   frame's; anything else renders. So a stale range can cost a render, never a wrong replay: a view that was
   not drawn last frame, one that was prepainted and not painted (`Window::transact`'s discarded attempts,
   which at the pin replay ranges of the discarded attempt), one whose state was written this very frame.
2. *A replayed range takes the views inside it along.* `Window::reuse_prepaint` / `reuse_paint` already
   walk the range's accessed element states to keep them alive; for the states that are a view's
   (`TypeId` of `ViewElementState`) they now move its ranges by the distance the content moved
   (`PrepaintStateIndex::rebase` / `PaintIndex::rebase`: every list is copied slice for slice, one entry for
   one, so one offset per list is exact) and stamp them with this frame. Views nested at any depth are in
   the range; views inside a deferred draw are in the deferred draw's own range, replayed (and rebased) in
   the deferred pass. A state already taken this frame is not in the previous frame's map and is left alone.
3. *The subtree is no longer forced.* `prepaint_view` leaves `window.refreshing` as it found it;
   `Window::refresh()` (theme, scale, window focus, the keyboard / pointer modality) still renders
   everything. `paint_view` is untouched: paint replays exactly the views prepaint replayed.
4. *The key gains the rem size* (`Window::with_rem_size` can override it for a subtree: a parent that
   rendered with another one must not find its children's old layout).
5. `gpui::set_nested_view_replay(false)` restores the pin's behaviour (the client: `DECIBELL_VIEW_REPLAY=0`),
   `CACHED_VIEWS_RENDERED` / `CACHED_VIEWS_REPLAYED` count, process-wide.

**What a replayed nested view assumes about the parent that rendered, and why it holds.** Nothing new:
a cached view under a parent that renders is how GPUI is used wherever the parent is *not* cached (the
window's root view renders every frame with its cached children replayed: Zed's panes under its workspace).
The patch makes a cached parent behave like that one.

| What the replay carries | Depends on, in the parent's new frame | Held by |
|---|---|---|
| layout, scene (quads, text, sprites), debug bounds | absolute bounds, content mask, inherited text style, opacity, rem size | the cache key: any of them changed → renders |
| hitboxes, mouse listeners, cursor styles, tooltips | the same; z-order is the order they are pushed in | pushed at the view's place in the parent's fresh order |
| dispatch-tree nodes (key contexts, action handlers, focus) | the parent's node they hang under | `DispatchTree::reuse_subtree` re-parents under the current node, and reports whether the focused node is inside (`next_frame.focus`) |
| the focused element's input handler | focus | taken from the previous frame with the range |
| deferred draws (popovers) | their own ranges | the deferred pass replays and rebases them |
| element states under the view | the global element id path | a changed path finds no state: the view renders |
| the entities the view read | the window's subscriptions | `accessed_entities`, restored on replay, as at the pin (and now also collected into the rendering parent's set, which is a superset of what it had) |
| line layouts | the font generation | kept per range; a new generation refuses the range (the pin's rule) |

Not in the key, at the pin and now: an image cache the parent provides (`with_image_cache`). A parent that
swaps it keeps the child's old images until the child renders; nothing in the client does that.

**Evidence.** `crates/decibell-harness/tests/nested_cached_views.rs` (GPUI's test window; `cargo test -p
decibell-harness`), four tests on a root with a side column and a row of three leaves, each leaf a cached
view with a click listener, a focus handle with a key context and an action, an input handler and a
deferred popover:

| | pin (patches to `0013`) | pin + the two-line change | `0014` |
|---|---|---|---|
| a clean view nested in a dirty one is replayed, and clicks, the focused action, typed text and the popover work | **fails**: the leaves render | passes | passes |
| the row is replayed whole while the side column grows or shrinks before it, then renders with clean leaves: every leaf's click, action, input and paint are its own (6 rounds, both orders, both in one frame) | fails (renders) | **panics** in `key_dispatch.rs` | passes |
| a leaf the row moved, clipped (two masks) or gave another text colour renders; left alone it is replayed; `Window::refresh` renders all | fails | passes | passes |
| a leaf that left the tree for a frame renders when it comes back | fails | passes | passes |

**Measured** (`decibell --selftest render-cost`, release build, one binary with the switch both ways, three
alternating passes of 20–30 changes each; main-thread CPU per change includes the event and the whole
frame; 2026-11-26, load average 6.0 falling to 3.7):

| One change | Views rendered, as the pin | With `0014`: rendered / replayed | Main-thread µs per change: pin → `0014` |
|---|---|---|---|
| A speaking ring on a 12-tile voice stage (server bar, channel list with rosters, user panel around it) | 18.2 | 5.0 / 13.0 | 2,653 → 2,087 (−21 %) |
| A speaking ring in the channel list's voice roster, a text channel open with its member list | 9.5 | 3.0 / 3.0 | 2,624 → 1,662 (−37 %) |
| A key typed in a field under the 10,000-message channel's list (the chat pane's shape; the composer is not in this tree) | 3.0 | 2.0 / 1.0 | 941 → 873 (−7 %) |
| A message arriving in that list | 3.0 | 2.0 / 1.0 | 1,021 → 977 (within the noise: see below) |

A second run on the final build at load 1.9: 18.0 → 5.7 views and 2,554 → 1,958 µs; 9.0 → 3.1 and 2,325 →
1,497 µs; 3.0 → 2.0 and 911 → 874 µs; 3.0 → 2.0 and 1,002 → 1,073 µs. So: a fifth to a third less main-thread
time where the clean neighbours are many, a few percent for a key typed over this fixture's cheap list, and
nothing measurable for a message arriving (the list that renders is the cost).

What renders now is the path from the window's content to the view that changed (content, main layout, pane,
stage, tile: 5; content, list host, field: 2); what is replayed are the path's clean children, and the views
inside those are not visited at all (the second row: 9.5 views became 3 rendered and 3 replayed). The time
saved is those views' `render`, layout and paint; the rest of a frame (the path's own work, the scene to the
renderer) is unchanged, so the gain is largest where the neighbours are many or heavy. The fixture's message
list renders in about 70 µs; the composer work measured 0.3–2.6 ms per list render in the real pane, which is
what a typed key no longer pays there (its `composer-typing-cost` self-test is the number to take after the
merge).

**Not covered.** The gain is views not rendered; a view that renders still lays out and paints its own
elements, and GPUI still marks every ancestor of a dirty view dirty, so the path from the window's content
down to the changed view renders (2–5 views here). Zed's own crates were not built against the patch (the
client does not build them).

**Files.** `gpui/src/view.rs` (+71 / −3), `gpui/src/window.rs` (+101), `gpui/src/text_system/line_layout.rs`
(+12).

**Upstream.** The stale-range hazard and the fix are GPUI's own; a version for upstream would drop the
switch and keep the counters behind `test-support`.
## 0015: Linux clipboard writes carry pictures

`gpui_linux/src/linux/wayland/{clipboard.rs,client.rs,serial.rs}`, `x11/{clipboard.rs,client.rs}` (+308 / −51). A
diff against the pin with `0001`–`0012` applied; it also applies after the `0013` / `0014` of the hide-cursor and
nested-view work (checked with `git apply` on the merged set).

**What.** At the pin GPUI read pictures from the Linux clipboards but wrote text only: Wayland offered the three text
types and its own marker type whatever the item held and answered every request with `item.text()`; X11 called
`set_text(item.text())` (its `set_image` was unused and offered every picture as `image/png`).

- **Wayland**: `write_to_clipboard` offers the text types only when the item has text, and each image entry under its
  own type (`ImageFormat::mime_type`: `image/png`, `image/jpeg`, …; `offered_mime_types`). `send` / `send_primary`
  answer a request with what was asked for (`contents_for_mime_type`: the text for a text type, the picture's bytes
  for its type, nothing otherwise). `send_bytes` makes the reader's pipe non-blocking first: a picture is larger than
  a pipe buffer, and a blocking write would have stalled the event loop until the reader took all of it; now it goes
  out from the loop's write readiness as the reader reads.
- **The selection serial**: GPUI took the selection only with the serial of a key or button press in one of its
  windows. A window that has the keyboard focus but has not been pressed in (a selection set from code right after
  start; a self-test, which has no real input) never could. `SerialTracker::selection_serial` now falls back to the
  serial of the `wl_keyboard.enter` that gave us the focus, as Qt does (its "last input serial" includes enter
  events). The focus condition (`mouse_focused_window || keyboard_focused_window`) is unchanged. KWin accepts it (it
  checks the keyboard focus, not the serial); wlroots accepts any serial it sent to the client; Mutter may refuse a
  serial older than the current selection's, which is no worse than not asking. A unit test covers the order.
- **X11**: `write_to_clipboard` owns the selection with every entry (`Clipboard::set_item`: `UTF8_STRING` for text,
  each picture under its type's atom); `set_image` uses the picture's own atom. Contents larger than 1 MiB
  (`INCR_CHUNK`) are served with ICCCM's `INCR`: the serving thread announces the size, watches the requestor's
  property deletions (`PropertyNotify`) and writes the next piece each time, then the empty property that ends it.
  Before, one `ChangeProperty` larger than the server's maximum request failed, and the error ended the serving thread
  (no clipboard for the rest of the process). A transfer whose requestor stops reading is dropped after 30 s; the
  clipboard-manager hand-over at quit counts an `INCR` transfer as written when its last piece went out and waits up to
  2 s for one in progress (else 100 ms, as before).

**Why.** "Copy image" (owner request, 2026-11-26): `decibell-chat-content`'s `actions::copy_image` puts a PNG on the
clipboard, as the Electron client's `clipboard.writeImage` does.

**Evidence.** `decibell --selftest content-copy-image` (`crates/decibell-app/src/copy_image.rs`): see `HANDOFF.md`
§1 (the platform row) for the run.

**Windows and macOS** were not changed: Windows writes the picture under its registered format ("PNG", "GIF", "JFIF")
plus a PNG and **no `CF_DIB`**, so programs that paste only bitmaps (Paint, older Office) get nothing (the Windows batch
in `HANDOFF.md`); macOS writes the picture's own pasteboard type (`public.png`).

**Upstream.** Yes: GPUI's own `ClipboardItem::new_image` did nothing on Linux. The serial fallback is a separate
question for upstream (their test pins the opposite).

## Considered and not patched

- **Worker threads.** GPUI starts one `Worker-N` per CPU (20 here). Limited to 4 CPUs (`taskset -c 0-3`) the
  process has 17 threads instead of 33 and the same memory (private 77.8 vs 78.0 MB, PSS 81.5 vs 81.7): idle
  workers cost nothing measurable, so no cap.
- **System fonts on Linux**: patched, see 0011.
- **Windows vsync thread** (`crates/gpui_windows/src/platform.rs`: wakes every vblank while idle): cannot be
  run here; its own task.

## 0001 in detail

(The video work's notes. Its commands build a scratch copy with `0001` alone; `tools/fetch-gpui.sh` is what the
client's build uses.)

### Applying (a scratch copy with `0001` alone)

```sh
# a working copy of the pin (never edit gpui-client/vendor/zed itself)
git clone --depth 1 file://$PWD/gpui-client/vendor/zed gpui-client/vendor/scratch/zed-patched
git -C gpui-client/vendor/scratch/zed-patched apply ../../../patches/0001-gpui-external-surfaces.patch
```

`git apply --check` against the pin passes (verified 2026-11-26). The patch touches only Apache-2.0 crates (7 files, +574 / -46):

| File | Lines | Change |
|---|---|---|
| `crates/gpui/src/scene.rs` | +29 | `PaintSurface` gets `corner_radii` and `source: ExternalSurface` on non-Apple targets; the `ExternalSurface` handle type |
| `crates/gpui/src/window.rs` | +35 | `Window::paint_external_surface` (non-Apple), `Window::request_present` |
| `crates/gpui_wgpu/src/wgpu_surface.rs` | +150 (new) | `WgpuSurface`, `WgpuSurfaceFrame`, `WgpuSurfacePlanes`, `SurfaceColor` |
| `crates/gpui_wgpu/src/wgpu_renderer.rs` | +232 / -23 | draws `PrimitiveBatch::Surfaces`; every queue use under `device_queue_lock` |
| `crates/gpui_wgpu/src/wgpu_context.rs` | +109 / -12 | `shared_context`, `set_vulkan_device_hook`, `device_queue_lock` |
| `crates/gpui_wgpu/src/shaders.wgsl` | +17 / -11 | surface shader: colour matrix and chroma offset as uniforms, rounded corners, premultiplied output |
| `crates/gpui_wgpu/src/gpui_wgpu.rs` | +2 | module and re-export |

### Regenerating `0001` after a change or a new pin

```sh
cd gpui-client/vendor/scratch/zed-patched
git add -N crates/gpui_wgpu/src/wgpu_surface.rs        # so the new file is part of `git diff`
git diff > ../../../patches/0001-gpui-external-surfaces.patch
```

On a new pin: clone it, `git apply --3way` the patch, fix `wgpu_renderer.rs` by hand if it conflicts (it is
the file upstream changes most), rebuild the prototype (`video-design.md` §13) and rerun its measurements
before trusting the result.

### Not patched by `0001`

`gpui_windows` (Direct3D 11) still ignores surfaces; the matching change, and the interface `decibell-video`
expects from it, is in `video-design.md` §8.
`gpui_apple` needs nothing: `Window::paint_surface` with a `CVPixelBuffer` already works there.
