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

## Considered and not patched

- **Worker threads.** GPUI starts one `Worker-N` per CPU (20 here). Limited to 4 CPUs (`taskset -c 0-3`) the
  process has 17 threads instead of 33 and the same memory (private 77.8 vs 78.0 MB, PSS 81.5 vs 81.7): idle
  workers cost nothing measurable, so no cap.
- **System fonts on Linux.** GPUI's text system loads the index of every installed font (1,095 faces here,
  10 ms), and cosmic-text then walks all of them for each new (family, weight, style) it shapes, opening most
  font files to look for a weight axis: 15 such queries for the gallery, 62.6 ms in total, about a quarter of
  the time to the first frame (`DECIBELL_LOG=debug`, "font matches for … in …"). A one-line patch
  (`CosmicTextSystem::new_without_system_fonts` in `crates/gpui_linux/src/linux/platform.rs`) removes both,
  but also every fallback for scripts our fonts lack; it needs a loader that adds a covering system font when
  a glyph is missing (`App::on_missing_glyphs`). Left for the text work to decide.
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

`gpui_windows` (Direct3D 11) still ignores surfaces; the matching change is designed in `video-design.md` §8.
`gpui_apple` needs nothing: `Window::paint_surface` with a `CVPixelBuffer` already works there.
