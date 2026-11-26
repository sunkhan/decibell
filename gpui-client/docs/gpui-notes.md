# GPUI notes — source survey at a pinned commit

A reference for the agents building the Decibell GPUI client. Everything here was read from the
source of one commit, not from memory or docs:

- **Pin:** `zed-industries/zed` `a3ce8d5e88e35697b2ba1cf9cb3c6816089b8281` (2026-11-26), shallow clone at
  `gpui-client/vendor/zed` (gitignored; `gpui-client/GPUI_PIN` holds the hash).
- **References:** `path:line` relative to the zed root (`crates/gpui/src/window.rs:1906`). External crates
  are cited as `[crate-version]/path:line` (from `~/.cargo/registry`); `gc/…` is the vendored gpui-component
  0.11.0 under `gpui-client/vendor/scratch/gpui-component-check/vendor/`. A bare `:NN` continues the file of
  the previous ref. File names that exist in several crates are sometimes left bare where the surrounding
  text says which one: bare `window.rs`, `platform.rs`, `text_system.rs`, `input.rs`, `svg.rs`, `animation.rs`,
  `executor.rs` mean `crates/gpui/src/…` (`elements/…` for `svg.rs`/`animation.rs`/`text.rs`/`uniform_list.rs`)
  unless the bullet is about one backend; `client.rs` is the Wayland (`crates/gpui_linux/src/linux/wayland/`)
  or X11 (`…/linux/x11/`) client named in the same bullet; `events.rs` / `dispatcher.rs` next to Windows
  text are `crates/gpui_windows/src/…`. All other refs were expanded to full paths and every `crates/…` ref
  was checked to exist with a line number inside the file. Line numbers are for this pin only.
- **Measured** numbers (§13) come from the hello-world in `gpui-client/vendor/scratch/hello/` on CachyOS,
  KDE Plasma Wayland, RTX 4080 + NVIDIA 620 proprietary, 280 Hz 2560×1440 monitor, Rust 1.100.
- Things marked *inferred* were concluded from reading code paths, not from a run.
- Licence rule: GPUI crates are Apache-2.0; most other Zed crates (`editor`, `ui`, `theme`, `workspace`,
  `zed`…) are GPL — never depend on or copy from them. Where this document cites a GPL file it is only to
  say "this is where Zed does it; don't copy".

## Contents
1. Crates, licences, features, how to depend
2. App / window lifecycle on Linux and Windows
3. Frame scheduling and idle behaviour
4. Executors, timers, and talking to our tokio core
5. Text
6. Lists and scrolling
7. Text input
8. Images and vectors
9. Styling primitives
10. Testing and capture
11. Windows backend facts
12. Gotchas, surprises, accessibility
13. Measured hello-world

## Facts that shape the architecture (short version)
- **Idle:** on Wayland GPUI is genuinely idle — 0 frames, 0 main-thread wakeups, 0 ms CPU over 30 s
  (§13). On X11 the main thread wakes at the monitor refresh rate (280/s here, ~0.4 % of a core) and on
  Windows a vsync thread wakes every vblank; neither draws. Both are one-file patches (§3.2, §3.7).
- **Memory:** on this NVIDIA box the floor is set by the driver, not GPUI. Default 162 MB RSS / 85 MB Pss;
  stopping wgpu from also initialising EGL/GL brings it to **110 MB RSS / 68 MB Pss / 62 MB private**, of
  which the app's own heap is ~19 MB and the binary ~14 MB (§13).
- **Text:** cosmic-text + swash on Linux, DirectWrite on Windows. **No variable-font axes** (Linux
  rasterises the default instance; Windows snaps to named instances — instance the fonts offline, or patch
  `crates/gpui_wgpu/src/cosmic_text_system.rs:525-530` + the font key), **no inline elements/images in
  text**, no letter-spacing, colour emoji only from a font named `NotoColorEmoji` on Linux. Height-for-width
  measurement without painting uses the same code path as the text element (`shape_text`). §5.
- **Lists:** `list()` anchors on (item, offset) so prepends don't move the content; heights are exact only
  for laid-out rows; we can own the wheel and drive the offset per frame. §6.
- **Text input:** GPUI ships only the IME protocol (`EntityInputHandler`) and an example field.
  `gpui-component` (Apache-2.0) builds against this pin with a one-word macro fix and has a complete
  rope/undo/IME composer, but costs +10.4 MB of binary and an idle 2 Hz thread wholesale — vendor its
  input engine only. §7.
- **Capture to PNG** without a display works on Linux via `HeadlessAppContext` + `capture_screenshot`
  (test-support feature); not on Windows. §10.
- **Platform gaps we build ourselves:** tray, single instance + `decibell://` deep links (`on_open_urls`
  never fires on Linux/Windows), Wayland attention requests, idle/lock detection, image cache eviction,
  GIF playback cost control. §2, §8.

## 1. Crates, licences, features, how to depend

### 1.1 The crate split at this pin
| Crate | Role | Licence |
|---|---|---|
| `gpui` (v0.2.2, `crates/gpui/Cargo.toml:1-9`) | core: app/entity model, elements, layout (taffy), scene, text-system front end, executors | Apache-2.0 (`crates/gpui/Cargo.toml:7`) |
| `gpui_platform` | picks the backend: `application()`, `headless()`, `linux(modes)`, `current_platform()` (`crates/gpui_platform/src/gpui_platform.rs:16-92`) | Apache-2.0 |
| `gpui_linux` | Wayland + X11 + headless backends, calloop event loop, dispatcher, portals, keyring, notifications | Apache-2.0 |
| `gpui_wgpu` | Linux renderer (wgpu, Vulkan or GL) **and** the Linux text system (cosmic-text + swash + font-kit matching) | Apache-2.0 |
| `gpui_windows` | Windows backend with its own Direct3D 11 + DirectComposition renderer and DirectWrite text system (no wgpu) | Apache-2.0 |
| `gpui_macros`, `gpui_shared_string`, `gpui_util`, `refineable` (+`derive_refineable`), `collections`, `sum_tree`, `scheduler`, `http_client` (trait only, no implementation: `crates/http_client/Cargo.toml` deps are `http`, `bytes`, `serde`…), `ztracing` (+`ztracing_macro`) | support crates pulled in transitively | all Apache-2.0 |
| `gpui_tokio` | optional tokio bridge (§4.4) | Apache-2.0 (`crates/gpui_tokio/Cargo.toml:6`) |

In-tree crates actually linked by the hello-world (from `cargo tree`): Linux = the 15 above minus `gpui_tokio`/`gpui_windows`; Windows = same with `gpui_windows` instead of `gpui_linux`/`gpui_wgpu`. **No GPL/AGPL/LGPL-only crate on either target** (562 crates on `x86_64-unknown-linux-gnu`, 333 on `x86_64-pc-windows-msvc`). Non-MIT/Apache licences present: `self_cell` (Apache-2.0 OR GPL-2.0-only — take Apache; via cosmic-text), `option-ext` (MPL-2.0, file-level copyleft; via `dirs` ← zed-font-kit), Unicode-3.0 (icu_*), Zlib, ISC, BSD-2/3, BSL-1.0 (ryu, dual), Unlicense (dual), CC0 (dunce, dual). The GPL crates of Zed (`editor`, `ui`, `theme`, `workspace`…) are not reachable from `gpui`.

### 1.2 Git / forked dependencies (Linux build)
- `wgpu` 30.0.0 from git `gfx-rs/wgpu` rev `cddb2f5` (`Cargo.toml:1018`, workspace dep) — not crates.io.
- `zed-font-kit` fork (`Cargo.toml:647`), `oo7` fork (keyring, `Cargo.toml:841`), `zed-xim` fork (`crates/gpui_linux/Cargo.toml:131-134`), `scap` fork (screen capture, optional, `Cargo.toml:903`).
- Workspace `[patch.crates-io]` (`Cargo.toml:1048-1068`) replaces `async-task` (waker-identity fix), `calloop` (zed fork), `khronos-egl` (EGL_SUCCESS panic fix) among others. **These patches do not apply to an out-of-tree consumer** — repeat them in your own `[patch.crates-io]` (done in `gpui-client/vendor/scratch/hello/Cargo.toml`). Windows needs only the `async-task` patch.

### 1.3 Features and what can be turned off
- `gpui` default = `["font-kit", "wayland", "x11", "windows-manifest"]` (`crates/gpui/Cargo.toml:20`); `font-kit = []` and `wayland = []` are empty flags (`:39-40`), `x11` only forwards to `scap?/x11` (`:41`). Depend with `default-features = false`.
- `gpui_platform` features `wayland`, `x11`, `screen-capture`, `inspector`, `test-support`, `bench-support` (`crates/gpui_platform/Cargo.toml:16-29`); on Windows it force-enables `gpui/windows-manifest` (`:71`, embeds a manifest via `embed-resource`, `crates/gpui/Cargo.toml:47,203`).
- `gpui_linux`: `wayland` / `x11` select backends (`crates/gpui_linux/Cargo.toml:16-48`); **both** pull `accesskit_unix` and `gpui_wgpu` with `font-kit` (`:19-20,37-38,64`). Always-on regardless of features: `oo7` keyring with `async-std` (`:76-79`), `notify-rust` (`:81`), `calloop`, `http_client`, `smol`.
- `screen-capture` (scap) is **off** unless requested (`crates/gpui/Cargo.toml:44`, `crates/gpui_linux/Cargo.toml:49`). `hot-reload`, `profiler`, `inspector`, `stacker`, `leak-detection` are off by default.
- Not switchable without patching gpui's manifests (features are additive and set in Zed's workspace): `image` with bmp/dds/exr/ff/gif/hdr/ico/jpeg/png/pnm/qoi/rayon/tga/tiff/webp (`Cargo.toml:671-686`), `resvg` with `text`/`system-fonts`/`memmap-fonts`/`raster-images` (`Cargo.toml:887-892`), `accesskit`, `oo7`, `notify-rust`, `ashpd` (`async-io`, `inhibit`, `open_uri`, `file_chooser`, `settings`; `Cargo.toml:549-555`), `icu_segmenter` with compiled data, `schemars`, `regex`, `chrono`. A pruned fork of `crates/gpui/Cargo.toml` + `crates/gpui_linux/Cargo.toml` is the only way to drop them (measured effect of the features we *can* turn off: §13).

### 1.4 How an out-of-tree app should depend on gpui at this pin
- **Path dependency into the pinned clone** (what the hello-world does, `gpui-client/vendor/scratch/hello/Cargo.toml`):
  ```toml
  [dependencies]
  gpui = { path = "../../zed/crates/gpui", default-features = false }
  gpui_platform = { path = "../../zed/crates/gpui_platform", default-features = false, features = ["wayland", "x11"] }
  [patch.crates-io]   # copy of the relevant lines of zed's Cargo.toml:1048-1068
  async-task = { git = "https://github.com/smol-rs/async-task", rev = "41fd3359c75e221bd8b187a8ee361aa309c1714c" }
  calloop = { git = "https://github.com/zed-industries/calloop", rev = "3759371fee14c40066d64777e2a36b6ffcc22590" }
  khronos-egl = { git = "https://github.com/zed-industries/khronos-egl", rev = "42214b36347b108fd9630ca6254b96dde59e04fe" }
  ```
  Copy Zed's `Cargo.lock` next to your manifest before the first build: cargo prunes it to the subset you use and you get exactly the versions Zed tested. Workspace inheritance (`edition.workspace = true`, `foo.workspace = true`) resolves against the clone's own root `Cargo.toml`, so keep the clone whole. Give the app its own `[workspace]` table so cargo never climbs into another workspace.
- **Git dependency**: `gpui = { git = "https://github.com/zed-industries/zed", rev = "a3ce8d5e88e35697b2ba1cf9cb3c6816089b8281", default-features = false }` works the same way (inheritance resolves inside the git checkout) but fetches the whole Zed repo, and the same `[patch.crates-io]` block is still needed.
- Zed's `[profile.*]` (`Cargo.toml:1082-1176`) and `rust-toolchain.toml` (channel 1.100.0) do not apply to us; we built with rustc 1.100.0. `gpui_platform` uses `cfg_select!` (`crates/gpui_platform/src/gpui_platform.rs:67`), so an old toolchain will not do.
- **crates.io `gpui` is not usable for this API**: the newest release is 0.2.2 of 2025-10-22 (13 months old, blade-graphics era, before the crate split and the wgpu move). The in-tree crate also says 0.2.2 but `gpui_platform`, `gpui_linux`, `gpui_wgpu`, `gpui_windows` are `publish.workspace` (unpublished).
- Entry point: `gpui_platform::application().run(|cx: &mut App| { … })` (`crates/gpui_platform/src/gpui_platform.rs:16-18`; example `crates/gpui/examples/hello_world.rs:94-116`). `gpui::Application::with_platform(Rc<dyn Platform>)` if you build the platform yourself.

## 2. App / window lifecycle on Linux and Windows

Source: zed `a3ce8d5e88e35697b2ba1cf9cb3c6816089b8281`. All paths relative to the zed root. "W" = Wayland, "X" = X11, "Win" = Windows.

Licensing: every crate on these paths is Apache-2.0 (`gpui`, `gpui_linux`, `gpui_windows`, `gpui_wgpu`, `gpui_platform`, and their in-tree deps `collections`, `sum_tree`, `scheduler`, `refineable`, `http_client`, `gpui_util`, `gpui_macros`, `gpui_shared_string`, `util_macros`, `zlog`, `ztracing`: each `crates/<name>/Cargo.toml` `license = "Apache-2.0"`). Third-party: `oo7` (zed git fork, MIT), `ashpd` (MIT), `notify-rust` (MIT/Apache), `accesskit_*` (MIT/Apache), `zed-xim` / `zed-scap` (MIT), `zed-font-kit` (MIT/Apache). **GPL code worth knowing about, but not to copy:** `crates/workspace` (Zed's client-side-decoration chrome), `crates/zed` (single-instance + URL hand-off), `crates/cli` are `GPL-3.0-or-later` (`crates/workspace/Cargo.toml:6`, `crates/zed/Cargo.toml:7`, `crates/cli/Cargo.toml:6`). The Apache-licensed reference for client-side decorations is `crates/gpui/examples/window_shadow.rs`.

### 2.1 Bootstrapping: `Application`, platform selection, `run`

- `Application::with_platform(Rc<dyn Platform>)` (`crates/gpui/src/app.rs:242`) and `Application::new_inaccessible(platform)` (AccessKit force-disabled, `crates/gpui/src/app.rs:259-263`). There is **no** `Application::new()` (searched `app.rs`; the only `new()` at `crates/gpui/src/app.rs:584` belongs to `SystemWindowTabController`).
- `gpui_platform::application()` → `current_platform(false)` (`crates/gpui_platform/src/gpui_platform.rs:16-18`); `headless()` → `current_platform(true)` (`:20-22`); `gpui_platform::linux(allowed_modes: WindowingModes)` (Linux/FreeBSD only, `:29-35`). `current_platform` picks `gpui_windows::WindowsPlatform::new(headless).expect(..)` on Windows (panics on failure) and `gpui_linux::current_platform(headless)` on Linux (`:62-82`).
- Linux: `current_platform(headless)` → `linux_platform(WindowingModes::HEADLESS)` if `headless` **or env `ZED_HEADLESS` is set**, else `WindowingModes::all()` (`crates/gpui_linux/src/linux.rs:40-46`). Backend choice: Wayland if `WAYLAND_DISPLAY` set, else X11 if `DISPLAY` set, else headless (`crates/gpui_linux/src/linux/display_connection.rs:23-38`). Initial connect happens inside `run`; failure to reach the named server **panics** (`crates/gpui_linux/src/linux/platform.rs:354-393`).
- Builder: `with_assets`, `with_restart_arguments`, `with_http_client`, `with_quit_mode(QuitMode)` (`crates/gpui/src/app.rs:266-294`), `with_windowing(WindowingRequest)` (`crates/gpui/src/app.rs:302-305`), `on_open_urls` / `on_reopen` (`crates/gpui/src/app.rs:359-380`).
- `Application::run(FnOnce(&mut App))` blocks in `Platform::run` (`crates/gpui/src/app.rs:312-329`). Linux: one calloop `EventLoop` for the process; foreground runnables are idle callbacks (`crates/gpui_linux/src/linux/platform.rs:187-227`, `:665-684`). Windows: classic `GetMessageW` loop (`crates/gpui_windows/src/platform.rs:589-611`).
- Windows startup also calls `OleInitialize` on the main thread (`crates/gpui_windows/src/platform.rs:188-190`): the UI thread is an **STA**; our native code must not `CoInitializeEx(MTA)` on it.

### 2.2 Windowing modes, headless toggling

- Types: `WindowingRequest::{Headless, Windowed(GraphicalEnvironment)}` (`crates/gpui/src/platform.rs:310-315`); `WindowingModes` bitflags `WAYLAND | X11 | HEADLESS` (Linux only, `platform.rs:378-389`); Linux `GraphicalEnvironment { wayland_display, x11_display, xdg_runtime_dir, current_desktop, activation_token }` (`platform.rs:225-245`); Windows `GraphicalEnvironment { session_id }` (`platform.rs:252-285`).
- `App::request_windowing(req) -> Task<Result<()>>` fails if any window is open (`crates/gpui/src/app.rs:1391-1398`). Linux implements a real disconnect/reconnect between Wayland/X11/headless, applied between loop iterations (`crates/gpui_linux/src/linux/platform.rs:480-557`); a headless Linux window lays out but draws nothing (`crates/gpui_linux/src/linux/headless.rs:9-12, 37-46`). Windows only attaches/detaches the D3D devices + vsync thread and refuses if the platform was created headless (`crates/gpui_windows/src/platform.rs:623-641`); a headless Windows platform cannot open windows (`:761-763`) and uses `NoopTextSystem` (`:193-197`). Windows starts headless automatically if the window station is non-interactive (SSH/service) (`:397-412`, `:1736-1776`).
- `App::graphical_environment()` (`crates/gpui/src/app.rs:1409-1411`) — apply it to child processes we spawn (`GraphicalEnvironment::apply_to`, `platform.rs:336-347`).

### 2.3 Quit, last window, shutdown

- `QuitMode::{Default, LastWindowClosed, Explicit}`; `Default` = quit when the last window closes on Linux/Windows (`crates/gpui/src/app.rs:524-534`, enforced at `crates/gpui/src/app.rs:3455-3463`). **A tray-resident app must use `QuitMode::Explicit`** (`with_quit_mode` `crates/gpui/src/app.rs:291`, `set_quit_mode` `crates/gpui/src/app.rs:2253`).
- `cx.quit()` → `platform.quit()` (`crates/gpui/src/app.rs:1374-1376`): Linux stops the calloop loop (`crates/gpui_linux/src/linux/platform.rs:686-688`); Windows posts `PostQuitMessage` from a foreground task (`crates/gpui_windows/src/platform.rs:613-617`). After the loop exits the platform's quit callback runs `App::shutdown` (`crates/gpui/src/app.rs:1224-1242`), which runs all `on_app_quit` futures with `SHUTDOWN_TIMEOUT = 200 ms` (`crates/gpui/src/app.rs:80`, `:1294-1339`). `on_app_quit` cannot veto (`crates/gpui/src/app.rs:2997-3010`; entity variant `crates/gpui/src/app/context.rs:195-213`). X11 additionally waits for the clipboard hand-over to a clipboard manager after the loop (`crates/gpui_linux/src/linux/platform.rs:682-683`).
- Windows session end: `WM_QUERYENDSESSION` → allow; `WM_ENDSESSION` → runs the quit callback then `std::process::exit(0)` (`crates/gpui_windows/src/platform.rs:1237-1238`, `:1290-1306`). Linux: no logout/session-end or signal handling (searched `SIGTERM|SIGINT|signal_hook|ctrlc|SetConsoleCtrlHandler` in gpui/gpui_linux/gpui_windows: not found).
- `cx.on_window_closed(|cx, WindowId|)` (`crates/gpui/src/app.rs:3024`); `App::restart` uses a bash re-launch script on Linux (`crates/gpui_linux/src/linux/platform.rs:706-756`) and PowerShell on Windows (`crates/gpui_windows/src/platform.rs:647-706`).

### 2.4 `cx.open_window` and `WindowOptions`

`App::open_window<V: Render>(WindowOptions, |&mut Window, &mut App| -> Entity<V>) -> Result<WindowHandle<V>>` (`crates/gpui/src/app.rs:1690-1727`; draws once synchronously before returning, `:1710-1716`). Also on `AsyncApp` (`crates/gpui/src/app/async_context.rs:199`). `WindowOptions` is at `platform.rs:2698-2783`, defaults at `:2889-2918`.

| Field (default) | Wayland | X11 | Windows |
|---|---|---|---|
| `window_bounds: Option<WindowBounds>` (None → centred on active/primary display) | size only; origin not settable (`crates/gpui_linux/src/linux/wayland/window.rs:582-583`). `Maximized`/`Fullscreen` → core calls `zoom`/`toggle_fullscreen` (`window.rs:1753-1757`) | origin+size (`crates/gpui_linux/src/linux/x11/window.rs:530-567`) | `SetWindowPlacement` (`crates/gpui_windows/src/window.rs:576-589`) |
| `titlebar: Option<TitlebarOptions{title, appears_transparent, traffic_light_position}>` (Some, not transparent) | title only (`appears_transparent` documented macOS/Windows only, `platform.rs:2926-2928`) | title → `WM_NAME`/`_NET_WM_NAME` (`crates/gpui_linux/src/linux/x11/window.rs:596-619`) | `appears_transparent=true` **or `titlebar: None`** → custom title bar (`hide_title_bar`, `crates/gpui_windows/src/window.rs:478-482`) |
| `focus: bool` (true) | ignored (no reference in `gpui_linux`) | ignored | `false` → `SW_SHOWNOACTIVATE` (`window.rs:578-583`) |
| `show: bool` (true) | ignored: Linux always maps (`window.rs:2435`) | ignored (`crates/gpui_linux/src/linux/x11/window.rs:1648-1654`) | `false` → created hidden; first `activate()` shows it (`window.rs:584-589`, `:367-390`, `:815-821`) |
| `kind: WindowKind` (Normal) | see 2.6 | see 2.6 | see 2.6 |
| `is_movable` (true) | ignored (`platform.rs:2806-2808`) | ignored | gates `HTCAPTION` (`events.rs:989-990`) |
| `app_owns_titlebar_drag` | macOS only (`platform.rs:2721-2732`) | — | — |
| `inactive_frame_interval` (33.3 ms), `low_power_frame_interval` (33.3 ms), `thermal_frame_interval` (16.7 ms) | core throttling, all platforms (`platform.rs:2734-2750`, `window.rs:1377-1403`, `:1980-1997`) — see 2.8 | same | same |
| `is_resizable` (true) | ignored by backend; only gates `start_window_resize` (`window.rs:2873-2877`) | same | `WS_THICKFRAME|WS_MAXIMIZEBOX` (`window.rs:497-499`) |
| `is_minimizable` (true) | ignored | ignored | `WS_MINIMIZEBOX` (`window.rs:501-503`) |
| `display_id` | layer-shell output only (`crates/gpui_linux/src/linux/wayland/client.rs:1342-1349`, `crates/gpui_linux/src/linux/wayland/window.rs:171`) | X screen index (`crates/gpui_linux/src/linux/x11/window.rs:488-490`) | monitor (`window.rs:518-524`) |
| `window_background` (Opaque) | see 2.7 | see 2.7 | see 2.7 |
| `app_id: Option<String>` | `xdg_toplevel.set_app_id` (`crates/gpui_linux/src/linux/wayland/window.rs:1935-1941`) — **required for `activate()`** (2.9) | `WM_CLASS` (`crates/gpui_linux/src/linux/x11/window.rs:1628-1646`) | no-op (default `platform.rs:1483`); use `cx.set_app_identity` |
| `window_min_size` | `set_min_size` (`crates/gpui_linux/src/linux/wayland/window.rs:286-288`) | `WM_NORMAL_HINTS` min; max = GPU max texture (`crates/gpui_linux/src/linux/x11/window.rs:789-806`) | `WM_GETMINMAXINFO` (`events.rs:208-221`) |
| `window_decorations: Option<WindowDecorations>` (None → `Server`, `window.rs:1749-1750`) | see 2.5 | see 2.5 | not used |
| `icon: Option<Arc<RgbaImage>>` | not used (icon from `.desktop` via app_id) | `_NET_WM_ICON` (`crates/gpui_linux/src/linux/x11/window.rs:808-829`) | not used; exe icon resource **ID 1** is loaded (`crates/gpui_windows/src/platform.rs:1702-1716`) |
| `tabbing_identifier`, `use_display_p3` | macOS only (`platform.rs:2778-2782`) | — | — |

Runtime window calls (all on `Window`): `set_window_title` (`window.rs:3451`), `set_app_id` (`:3469`), `set_background_appearance` (`:3474`), `minimize_window` (`:7285`), `zoom_window` (`:3397`), `toggle_fullscreen` (`:7290`), `resize` (`:3296`; async on Windows, `crates/gpui_windows/src/window.rs:655-678`), `remove_window` (close, `window.rs:2748`), `bounds`/`window_bounds`/`is_maximized`/`is_fullscreen` (`:3255`, `:2891`, `:2845`, `:3301`). **There is no hide/unhide-window API**: `App::hide` is log-only on Linux (`crates/gpui_linux/src/linux/platform.rs:762-764`) and a no-op on Windows (`crates/gpui_windows/src/platform.rs:710`); `hide_other_apps`/`unhide_other_apps` are `unimplemented!()` (panic) on Windows (`crates/gpui_windows/src/platform.rs:712-720`).

### 2.5 Decorations: CSD on Linux, custom title bar on Windows

Types: `WindowDecorations::{Server, Client}` (request, `platform.rs:871-877`), `Decorations::{Server, Client{tiling: Tiling}}` (actual, `:881-890`), `Tiling{top,left,right,bottom}` (`:1058-1084`), `ResizeEdge` (8 values, `:850-867`), `WindowControls{fullscreen,maximize,minimize,window_menu}` (`:894-915`), `WindowButtonLayout` parsed from GNOME `button-layout` (`:956-1054`).

Linux CSD API: `window.request_decorations` (`window.rs:2850`), `window.window_decorations()` (`:3431`), `set_client_inset(px)` (invisible shadow/resize margin, `:3415`), `set_client_corner_radii` (`:3421`), `start_window_move()` (`:3410`), `start_window_resize(edge)` (`:2873`), `show_window_menu(pos)` (`:3402`), `window_controls()` (`:3446`), `cx.button_layout()` + `observe_button_layout_changed` (`crates/gpui/src/app.rs:1978`, `window.rs:2698`).

| | Wayland | X11 | Windows |
|---|---|---|---|
| Server decorations | via `zxdg_decoration_manager_v1`; **if the compositor lacks it, GPUI silently falls back to Client** and the app must draw everything (`crates/gpui_linux/src/linux/wayland/window.rs:2215-2234`, binding `:291-296`). (Mutter/GNOME is known not to offer xdg-decoration — external knowledge, not in this tree.) | `_MOTIF_WM_HINTS` (`crates/gpui_linux/src/linux/x11/window.rs:1949-2001`) | native frame |
| Client decorations | `Decorations::Client{tiling}` from configure states (`crates/gpui_linux/src/linux/wayland/window.rs:1254-1289`, `:2205-2213`); surface is non-opaque in CSD (`:650-653`, `:2485-2503`) | only if a compositor **and** `_GTK_FRAME_EXTENTS` support exist, else forced Server (`crates/gpui_linux/src/linux/x11/client.rs:419-427`, `crates/gpui_linux/src/linux/x11/window.rs:1952-1959`, `:1873-1900`) | n/a: use `appears_transparent` |
| `set_client_inset` | input/opaque/blur geometry (`crates/gpui_linux/src/linux/wayland/window.rs:2240-2247`) | `_GTK_FRAME_EXTENTS`, zeroed per tiled edge (`crates/gpui_linux/src/linux/x11/window.rs:1902-1947`) | no-op (default `platform.rs:1490`) |
| `start_window_move` / `start_window_resize` | `xdg_toplevel.move/resize` with last press serial (`crates/gpui_linux/src/linux/wayland/window.rs:2124-2150`) | `_NET_WM_MOVERESIZE` (`crates/gpui_linux/src/linux/x11/window.rs:1017-1047`, `:1863-1871`) | no-op (defaults `platform.rs:1468`, `:1475`) |
| `show_window_menu` | `xdg_toplevel.show_window_menu` (`crates/gpui_linux/src/linux/wayland/window.rs:2111-2122`) | `_GTK_SHOW_WINDOW_MENU` (`crates/gpui_linux/src/linux/x11/window.rs:1827-1861`) | no-op default |
| `window_controls()` | from `xdg_toplevel.wm_capabilities` (`crates/gpui_linux/src/linux/wayland/window.rs:1318-1347`) | default all-true | default all-true |
| Button layout | GNOME `button-layout` via portal Settings (`crates/gpui_linux/src/linux/xdg_desktop_portal.rs:71-76`, `:117-128`) | same (`crates/gpui_linux/src/linux/x11/client.rs:616-624`) | `None` (default `platform.rs:470-472`) |

Windows custom title bar: mark elements with `.window_control_area(WindowControlArea::{Drag, Close, Max, Min})` (`crates/gpui/src/elements/div.rs:1304-1307`; enum `window.rs:765-774`). Core answers the platform hit-test from the last frame's hitboxes (`window.rs:2305-2326`); `WM_NCHITTEST` maps them to `HTCAPTION`/`HTCLOSE`/`HTMAXBUTTON`/`HTMINBUTTON` (`crates/gpui_windows/src/events.rs:976-1042`). Returning `HTMAXBUTTON` is what gives Windows 11 the **snap-layouts flyout**. Min/Max/Close clicks are executed on `WM_NCLBUTTONUP` (`events.rs:1114-1178`). With the caption hidden, `WM_NCCALCSIZE` removes only the top non-client area (other borders stay native, so the DWM shadow and edge resize remain), and the top resize band is hit-tested manually (`events.rs:777-798`, `:1008-1039`). Double-click on a `Drag` area falls through to `DefWindowProc` (maximize toggle) (`events.rs:115-120`). Linux backends ignore `on_hit_test_window_control` (`crates/gpui_linux/src/linux/wayland/window.rs:2039-2040`, `crates/gpui_linux/src/linux/x11/window.rs:1771-1772`).

### 2.6 Window kinds (`WindowKind`, `platform.rs:2936-2963`)

| Kind | Wayland | X11 | Windows |
|---|---|---|---|
| `Normal` | xdg_toplevel | normal | `WS_EX_APPWINDOW` (`window.rs:504-509`) |
| `PopUp` ("above all others") | **plain xdg_toplevel**, no always-on-top (`crates/gpui_linux/src/linux/wayland/window.rs:258-263`) | `override_redirect` + `_NET_WM_WINDOW_TYPE_NOTIFICATION` (`crates/gpui_linux/src/linux/x11/window.rs:519`, `:621-632`) | `WS_EX_TOOLWINDOW|WS_EX_TOPMOST` (no taskbar entry) (`window.rs:492-493`) |
| `Floating` | `set_parent(keyboard-focused window)` (`crates/gpui_linux/src/linux/wayland/window.rs:266-268`, `crates/gpui_linux/src/linux/wayland/client.rs:1339`) | `WM_TRANSIENT_FOR` focused window (`crates/gpui_linux/src/linux/x11/window.rs:634-651`) | **no special handling** (not referenced in `gpui_windows`) |
| `Dialog` | parent + `xdg_dialog_v1.set_modal` if available (`crates/gpui_linux/src/linux/wayland/window.rs:270-284`) | transient + `_NET_WM_WINDOW_TYPE_DIALOG` + `_NET_WM_STATE_MODAL` (`crates/gpui_linux/src/linux/x11/window.rs:653-690`) | disables the active window, `WS_POPUP|WS_CAPTION`, `WS_EX_DLGMODALFRAME` (`window.rs:464-477`, `:504-506`) |
| `AnchoredPopup(PopupOptions)` (menus/tooltips outside the window) | native `xdg_popup` with positioner + optional grab (`crates/gpui_linux/src/linux/wayland/window.rs:210-256`) | **rejected** `PopupNotSupportedError` (`crates/gpui_linux/src/linux/x11/window.rs:482-486`) | **rejected** (`crates/gpui_windows/src/window.rs:443-447`) |
| `LayerShell(LayerShellOptions)` (`#[cfg(linux, wayland)]`) | needs `zwlr_layer_shell_v1`, else `LayerShellNotSupportedError` (`crates/gpui_linux/src/linux/wayland/window.rs:164-208`) | n/a | n/a |

### 2.7 Background appearance (`WindowBackgroundAppearance`, `platform.rs:2996-3016`)

- Wayland: non-Opaque → transparent swapchain (`crates/gpui_linux/src/linux/wayland/window.rs:2466-2470`); `Blurred` via `ext-background-effect-v1`, else KDE `org_kde_kwin_blur` (`:2505-2547`); Mica variants ignored (fall to "not Blurred").
- X11: non-Opaque → transparent renderer only (ARGB visual, `crates/gpui_linux/src/linux/x11/window.rs:298-307`, `:492-500`); **no blur protocol** (searched `BLUR|blur` in `x11/`: not found).
- Windows: `Transparent`/`Blurred` via undocumented `SetWindowCompositionAttribute` (accent 2/4) (`crates/gpui_windows/src/window.rs:916-941`, `:1628-1676`); `MicaBackdrop`/`MicaAltBackdrop` via `DWMWA_SYSTEMBACKDROP_TYPE` (build 22621+) (`:1604-1626`). Transparency needs DirectComposition (11.2).

### 2.8 Frame-rate limits that matter for video

`on_request_frame` applies the longest of: `inactive_frame_interval` while the window is **not OS-active** (and no high-rate input), `low_power_frame_interval` while `PowerMode::LowPower`, `thermal_frame_interval` under Serious/Critical thermal state (`window.rs:1377-1403`, `:1980-2049`). Defaults cap an unfocused window to ~30 fps (`platform.rs:2903-2905`). A stream viewer in a background window, or any window while Linux power-profiles-daemon reports `power-saver` / Windows Battery Saver is on (2.21), renders at 30 fps unless we set these fields to `None`. Mandatory presentations bypass the limits (`window.rs:1980`).

### 2.9 Multiple windows, activation, focus, visibility

- Multiple windows: `cx.windows()` (`crates/gpui/src/app.rs:1632`), `cx.active_window()` (`:1649`), `cx.window_stack()` (`:1644`: X11 only via `_NET_CLIENT_LIST_STACKING` `crates/gpui_linux/src/linux/x11/client.rs:1895-1933`; `None` on Wayland `crates/gpui_linux/src/linux/wayland/client.rs:1524-1526` and Windows (default `platform.rs:430-432`)).
- Active status: `window.is_window_active()` (`window.rs:3377`), `cx.observe_window_activation(window, cb)` (`crates/gpui/src/app/context.rs:410-422`); wired on Wayland (`crates/gpui_linux/src/linux/wayland/window.rs:2007`), X11 (`crates/gpui_linux/src/linux/x11/window.rs:1739`), Windows (deferred to the executor, `events.rs:800-847`). `is_window_hovered` (`window.rs:3384-3394`).
- Visibility: `WindowVisibility::{Visible, Hidden}` + `observe_window_visibility` (`platform.rs:86-114`, `window.rs:2660-2683`, `crates/gpui/src/app/context.rs:426`). Wayland = xdg-shell v6 `suspended` only (`crates/gpui_linux/src/linux/wayland/window.rs:1269`); X11 = map state + `VisibilityNotify`; Windows = shown & not minimized/cloaked, a fully covered window stays `Visible` (`platform.rs:97-100`, `crates/gpui_windows/src/window.rs:262-268`).
- `window.activate_window()` (`window.rs:7275`):
  - Wayland: requests an xdg-activation token tied to the last input serial and activates; **does nothing if the window has no `app_id`** (`crates/gpui_linux/src/linux/wayland/window.rs:1893-1913`). Without a recent press the compositor won't focus, but KWin/Mutter then mark the window as demanding attention (comment `:1902-1904`).
  - X11: `_NET_ACTIVE_WINDOW` client message, source=application, `CurrentTime` (`crates/gpui_linux/src/linux/x11/window.rs:1561-1579`).
  - Windows: restores if iconic, `SetActiveWindow`/`SetFocus`, then **synthesizes an Alt press+release with `SendInput`** before `SetForegroundWindow` (`crates/gpui_windows/src/window.rs:815-867`). Our `GetAsyncKeyState`-polled hotkeys can observe that synthetic Alt.
  - `cx.activate(..)` (app-level) is log-only on Linux (`crates/gpui_linux/src/linux/platform.rs:758-760`) and a no-op on Windows (`crates/gpui_windows/src/platform.rs:708`).
- Startup focus on Wayland: `XDG_ACTIVATION_TOKEN` is taken from the env at platform creation and consumed by the first toplevel (`crates/gpui_linux/src/linux/wayland/client.rs:202-210`, `:563-571`, `:1367-1369`). **No API accepts a token forwarded from a second process** to activate an existing window (searched `activation_token` in `crates/gpui/src`: only the `GraphicalEnvironment` field used when (re)connecting).

### 2.10 Attention / urgency (`window.request_attention()`, `window.rs:7280`)

| Wayland | X11 | Windows |
|---|---|---|
| **no-op** (`crates/gpui_linux/src/linux/wayland/window.rs:1915`); xdg-activation is used only for `activate()`/open_uri. Workaround: `activate_window()` while unfocused → "demands attention" on KWin/Mutter (`:1902-1904`) | ICCCM `WM_HINTS` urgency if not active (`crates/gpui_linux/src/linux/x11/window.rs:410-441`, `:1581-1587`); cleared on focus gain (`:1154-1158`) | `FlashWindowEx(FLASHW_ALL, uCount=1)` if not active (`crates/gpui_windows/src/window.rs:869-888`) |

### 2.11 Close interception

`window.on_window_should_close(cx, |window, cx| -> bool)` (`window.rs:7520-7529`; one callback per window, a new call replaces it). Wired: Wayland `xdg_toplevel.close` (`crates/gpui_linux/src/linux/wayland/window.rs:1304-1316`), X11 `WM_DELETE_WINDOW` (`crates/gpui_linux/src/linux/x11/window.rs:692-701`, `crates/gpui_linux/src/linux/x11/client.rs:915`), Windows `WM_CLOSE` (`events.rs:320-325`). Not consulted by `cx.quit()` or `remove_window()`. Minimize-to-tray = return `false` and close/hide ourselves (but see 2.4: no hide API, so "hide" = `remove_window()` + reopen later, with `QuitMode::Explicit`).

### 2.12 Single instance, `on_open_urls`, deep links (`decibell://`), jump list

- `Application::on_open_urls(FnMut(Vec<String>))` (`crates/gpui/src/app.rs:359-365`) is **stored but never invoked** on Linux (`crates/gpui_linux/src/linux/platform.rs:818-820`; only writer of `callbacks.open_urls`) or Windows (`crates/gpui_windows/src/platform.rs:793-795`; same). `on_reopen` likewise (`crates/gpui_linux/src/linux/platform.rs:979-983`, `crates/gpui_windows/src/platform.rs:863-865`).
- `cx.register_url_scheme(..)` returns `Err("register_url_scheme unimplemented")` on Linux (`crates/gpui_linux/src/linux/platform.rs:1168-1170`) and Windows (`crates/gpui_windows/src/platform.rs:1146-1148`).
- Single-instance: not in GPUI (searched `single_instance|CreateMutex|NamedPipe` in gpui crates: not found). Zed does it in GPL code: Unix datagram socket in the data dir (`crates/zed/src/zed/open_listener.rs:415-438`) and a Win32 mutex + named pipe (`crates/zed/src/zed/windows_only_instance.rs:29-104`). **We must build: scheme registration (`.desktop` `x-scheme-handler/decibell`; HKCU `Software\Classes\decibell`), argv parsing, IPC to the running instance, and activation (Wayland token limitation in 2.9).**
- Windows jump list: `cx.update_jump_list`/`set_dock_menu` build an `ICustomDestinationList` whose tasks launch `<exe> --dock-action N` as a **new process** (`crates/gpui_windows/src/destination_list.rs:122`, `:193-195`); the app must forward N to the running instance and call `cx.perform_dock_menu_action` (`crates/gpui/src/app.rs:3075`, `crates/gpui_windows/src/platform.rs:1150-1160`). Linux `set_dock_menu` is a `todo(linux)` no-op (`crates/gpui_linux/src/linux/platform.rs:1060-1062`).

### 2.13 Menus, tray, badges

- `cx.set_menus(..)` (`crates/gpui/src/app.rs:3059`): Linux and Windows only store the menus for `get_menus()` (`crates/gpui_linux/src/linux/platform.rs:1050-1058`, `crates/gpui_windows/src/platform.rs:959-965`); no native menu bar or D-Bus global menu. Apps render their own menu bar.
- System tray / status icon: **not found** (searched `tray|StatusNotifierItem|Shell_NotifyIcon|NOTIFYICONDATA|NSStatusBar` in all gpui crates).
- Taskbar/dock badges and overlay icons: **not found** (searched `badge|ITaskbarList3|SetOverlayIcon|LauncherEntry`).

### 2.14 Clipboard

Types: `ClipboardItem{entries: Vec<ClipboardEntry>}` (`platform.rs:3201-3204`), `ClipboardEntry::{String(ClipboardString), Image(Image), ExternalPaths}` (`:3242-3249`), `Image{format: ImageFormat, bytes, id}` (`:3453-3460`), `ImageFormat::{Png,Jpeg,Webp,Gif,Svg,Bmp,Tiff,Ico,Pnm}` (`:3376-3398`). API: `cx.read_from_clipboard()`/`write_to_clipboard` (`crates/gpui/src/app.rs:1988`, `:2022`), `read_from_clipboard_async` (default = sync, `platform.rs:641-643`), `read_from_primary`/`write_to_primary` (Linux only, `crates/gpui/src/app.rs:2028-2038`).

| | Wayland | X11 | Windows |
|---|---|---|---|
| Write text | yes; needs a focused window and a recent key/pointer press serial or it is skipped (`crates/gpui_linux/src/linux/wayland/client.rs:1482-1506`) | yes (`crates/gpui_linux/src/linux/x11/client.rs:1845-1857`) | `CF_UNICODETEXT` (CRLF) + GPUI metadata (`crates/gpui_windows/src/clipboard.rs:180-199`) |
| Write image | **no**: only text MIME types are offered and only `text()` is sent (`crates/gpui_linux/src/linux/wayland/clipboard.rs:17-19`, `:177-191`) | **no**: writes `item.text()` only; `set_image` exists but is unused (`crates/gpui_linux/src/linux/x11/client.rs:1849-1853`, `crates/gpui_linux/src/linux/x11/clipboard.rs:1012`) | yes: native PNG/GIF/JFIF/SVG + an extra PNG; **no `CF_DIB`** (`clipboard.rs:201-221`) |
| Read text / image | text (utf-8) first, then first matching image MIME (`crates/gpui_linux/src/linux/wayland/clipboard.rs:98-135`, `:193-209`) | images first, then text (`crates/gpui_linux/src/linux/x11/clipboard.rs:1026-1060`) | text, image (incl. `CF_DIB`→BMP), files (`CF_HDROP`) (`clipboard.rs:92-128`, `:253-280`) |
| Read blocks UI thread | yes, pipe read up to 4 s (`crates/gpui_linux/src/linux/platform.rs:1333-1370`) | yes, up to 4 s (`crates/gpui_linux/src/linux/x11/clipboard.rs:102-105`) | — |
| Primary selection | text write; text/image read (`crates/gpui_linux/src/linux/wayland/client.rs:1457-1480`) | text write (`crates/gpui_linux/src/linux/x11/client.rs:1832-1843`) | n/a |

X11 clipboard (adapted from arboard, `crates/gpui_linux/src/linux/x11/clipboard.rs:1-15`) serves requests on its own thread (`:957-980`) and hands contents to a clipboard manager on drop (`:1081-1130`).

### 2.15 File dialogs and prompts

- `cx.prompt_for_paths(PathPromptOptions{files, directories, multiple, prompt})` / `cx.prompt_for_new_path(dir, suggested_name)` (`crates/gpui/src/app.rs:2189-2208`; options `platform.rs:3032-3041` — **no file-type filters**). `can_select_mixed_files_and_dirs()` is false on both (`crates/gpui_linux/src/linux/platform.rs:945-948`, `crates/gpui_windows/src/platform.rs:826-829`).
- Linux (W and X): xdg-desktop-portal FileChooser via `ashpd`, modal to the focused window; returns `Err("...missing xdg-desktop-portal implementation.")` if no portal (`crates/gpui_linux/src/linux/platform.rs:60-61`, `:822-943`).
- Windows: `IFileOpenDialog`/`IFileSaveDialog` on a dedicated `windows-native-dialog` thread per dialog, owner = active GPUI window; save filter fixed to "All files (*.*)" (`crates/gpui_windows/src/platform.rs:797-824`, `:1600-1700`; `crates/gpui_windows/src/dialog.rs:85-154`).
- `window.prompt(level, msg, detail, answers, cx)` (`window.rs:7317-7353`): Linux backends return `None` (`crates/gpui_linux/src/linux/wayland/window.rs:1883-1891`, `crates/gpui_linux/src/linux/x11/window.rs:1551-1559`) → GPUI's in-window `fallback_prompt_renderer` or a custom `cx.set_prompt_builder` (`crates/gpui/src/window/prompts.rs:73`, `:215-232`; `crates/gpui/src/app.rs:3259`). Windows uses a native `TaskDialogIndirect` (`crates/gpui_windows/src/window.rs:721-813`).

### 2.16 Drag and drop

- Into a window: the platform emits `FileDropEvent::{Entered{position, paths}, Pending, Submit, Exited, Ended}` (`crates/gpui/src/interactive.rs:758-780`); core turns it into an internal drag of `ExternalPaths` (`crates/gpui/src/interactive.rs:715`, `window.rs:6283-6332`; `Submit` also calls `cx.activate(true)`). Elements use `.drag_over::<ExternalPaths>`, `.on_drop::<ExternalPaths>`, `.on_file_drop_exit` (`crates/gpui/src/elements/div.rs:1238`, `:1276`, `:1057`). **File paths only**: Wayland reads `text/uri-list` (`crates/gpui_linux/src/linux/wayland/client.rs:2914`, `:3028`), X11 XDND (`crates/gpui_linux/src/linux/x11/client.rs:932-1031`, `XdndAware` `:1736-1746`), Windows OLE `IDropTarget` with `CF_HDROP` only (`crates/gpui_windows/src/window.rs:1157-1214`). Dropped image data/URLs from a browser are not delivered (no other formats queried).
- Dragging out: `.external_drag_payload(|v, ..| Some(ExternalDragPayload::Files(FileDragPaths::new(..))))` (`crates/gpui/src/elements/div.rs:665`; types `crates/gpui/src/interactive.rs:727-747`); promoted when the drag leaves the window (`window.rs:6395-6422`). Supported **only on Wayland** (`crates/gpui_linux/src/linux/wayland/window.rs:2132-2139`, `crates/gpui_linux/src/linux/wayland/client.rs:673-711`); X11 and Windows return `false` (default `platform.rs:1469-1474`; no override found in `x11/` or `gpui_windows`).

### 2.17 Opening URLs, revealing paths

- `cx.open_url` (`crates/gpui/src/app.rs:2099`): Linux tries `open::commands` (xdg-open, gio, …) with the session env, then portal OpenURI (`crates/gpui_linux/src/linux/platform.rs:1194-1250`); Wayland first obtains an xdg-activation token so the browser can take focus (`crates/gpui_linux/src/linux/wayland/client.rs:1423-1438`, `:1943-1949`); X11 without a token (`crates/gpui_linux/src/linux/x11/client.rs:1814-1821`). Windows `ShellExecuteW("open")` (`crates/gpui_windows/src/platform.rs:779-791`, `:1537-1557`).
- `cx.reveal_path` (`crates/gpui/src/app.rs:2211`): Linux portal OpenDirectory, then open commands on the parent (`crates/gpui_linux/src/linux/platform.rs:1253-1284`); Windows `SHOpenFolderAndSelectItems` (`crates/gpui_windows/src/platform.rs:1559-1598`). `cx.open_with_system(path)`: Linux runs `xdg-open` (`crates/gpui_linux/src/linux/platform.rs:954-971`), Windows `ShellExecuteW` (`crates/gpui_windows/src/platform.rs:845-857`).

### 2.18 Credentials / keyring

`cx.write_credentials(url, username, &[u8])`, `read_credentials(url) -> Option<(String, Vec<u8>)>`, `delete_credentials(url)`, all `Task<Result<..>>` on the background executor (`crates/gpui/src/app.rs:2061-2096`).
- Linux: `oo7::Keyring` (Secret Service over D-Bus, `native_crypto`; `crates/gpui_linux/Cargo.toml:76-81`); items keyed by attributes `url`+`username` with a **hard-coded label `"zed-github-account"`**, and reads/deletes match only that label (`crates/gpui_linux/src/linux/platform.rs:57`, `:1086-1150`). Works for us, but our items will show under that label in Seahorse/KWallet.
- Windows: Credential Manager `CRED_TYPE_GENERIC`, target **`"zed:url=<url>"`**, `CRED_PERSIST_LOCAL_MACHINE`; secrets above `CRED_MAX_CREDENTIAL_BLOB_SIZE` are rejected up front (`crates/gpui_windows/src/platform.rs:1053-1144`, `crates/gpui_windows/src/util.rs:85-87`).
- Fine for one small secret per URL (saved password / resume token); E2EE key material larger than the Windows blob limit needs another store.

### 2.19 System notifications

API: `cx.set_app_identity(id, name)` (call early), `cx.show_system_notification(SystemNotification{tag, title, body, actions: Vec<SystemNotificationAction{id,label}>})`, `cx.dismiss_system_notification(tag)`, `cx.on_system_notification_response(|SystemNotificationResponse{tag, action_id}, cx|)` (`crates/gpui/src/app.rs:2119-2158`; types `platform.rs:722-754`; example `crates/gpui/examples/system_notifications.rs:45`, `:66`, `:106`, `:112`).
- Linux: `notify-rust` over `org.freedesktop.Notifications`; **one OS thread per notification**, blocked until it closes (`crates/gpui_linux/src/linux/system_notifications.rs:38-87`); body click = `"default"` action (`:14-16`); `app_name` from `set_app_identity` (`crates/gpui_linux/src/linux/platform.rs:999-1001`); **dismiss is a no-op** (`system_notifications.rs:89-93`); no icon/sound/urgency hints set.
- Windows: WinRT toasts (`ToastGeneric`, action buttons, **`audio silent="true"`**); unpackaged apps need `set_app_identity`, which sets the process AUMID and writes `HKCU\Software\Classes\AppUserModelId\<id>\DisplayName` (`crates/gpui_windows/src/platform.rs:913-929`; `crates/gpui_windows/src/system_notifications.rs:129-154`, `:157-197`). Clicks arrive via the in-process `Activated` handler (`:67-83`); no COM activator was found, so clicking a toast after the app exited does not relaunch it. Same-tag toasts replace; dismiss works (`:85-103`).

### 2.20 Screen capture (`screen-capture` feature, `scap`)

`cx.is_screen_capture_supported()` / `cx.screen_capture_sources()` (`crates/gpui/src/app.rs:1760-1769`; traits `platform.rs:797-814`). Without the feature: error (`platform.rs:434-448`). Uses Zed's `scap` fork (`Cargo.toml:902-903`): **displays only** (window targets dropped, `crates/gpui/src/platform/scap_screen_capture.rs:59-69`), fixed 60 fps YUV frames (`:201-207`), one blocking thread per stream (`:94-96`). X11 and Windows wired (`crates/gpui_linux/src/linux/x11/client.rs:1677-1689`, `crates/gpui_windows/src/platform.rs:738-748`); **Wayland returns `Err("Wayland screen capture not yet implemented.")` while `is_screen_capture_supported()` returns `true`** (`crates/gpui_linux/src/linux/wayland/client.rs:1287-1308`). Decibell's own portal/PipeWire and DXGI/WGC capture remain the way to go.

### 2.21 Power, sleep, idle, lock, thermal

| API | Linux | Windows |
|---|---|---|
| `cx.on_system_sleep` / `on_system_wake` (`crates/gpui/src/app.rs:1864`, `:1921`) | logind `PrepareForSleep` on the system bus (`crates/gpui_linux/src/linux/platform.rs:262-288`) | `WM_POWERBROADCAST` `PBT_APMSUSPEND`/`PBT_APMRESUMEAUTOMATIC` (`crates/gpui_windows/src/platform.rs:1417-1438`, `:867-884`) |
| `cx.power_mode()` → `PowerMode::{Unknown, LowPower, Balanced, HighPower}` (`platform.rs:758-768`) | power-profiles-daemon `ActiveProfile` (both bus names) (`crates/gpui_linux/src/linux/power.rs:13-86`, `crates/gpui_linux/src/linux/platform.rs:610-636`) | Battery Saver only: `LowPower` else `HighPower` (`crates/gpui_windows/src/platform.rs:886-911`, `:1453-1468`) |
| `cx.thermal_state()` / `on_thermal_state_change` | always `Nominal`, callback never fires (`crates/gpui_linux/src/linux/platform.rs:604-608`) | same (`crates/gpui_windows/src/platform.rs:576-580`) |
| `cx.prevent_idle_sleep(reason) -> Task<Result<ActivityGuard>>` (`crates/gpui/src/app.rs:1845`) | portal `Inhibit` with **`Suspend` flag only** (not Idle/screensaver), 10 s acquisition timeout (`crates/gpui_linux/src/linux/platform.rs:645-663`, `:1718-1765`) | `PowerRequestSystemRequired` (not `DisplayRequired`) (`crates/gpui_windows/src/platform.rs:141-184`, `:582-587`) |
| User idle time, lock/unlock, screensaver | **not found** (searched `GetLastInputInfo|WTSRegisterSessionNotification|SESSION_LOCK|LockedHint|ScreenSaver|ext_idle|IdleMonitor`) | **not found** |

Decibell's native `system_idle.rs` (ext-idle-notify) and lock detection must stay ours.

### 2.22 Displays, refresh rate, scale

- `cx.displays()`, `cx.primary_display()`, `cx.find_display(id)`, `cx.observe_displays(|DisplayEvent, cx|)` with `Added/Removed/Changed{BOUNDS|REFRESH_INTERVAL}` (`crates/gpui/src/app.rs:1750-1777`, `:1910`; `platform.rs:127-151`); `PlatformDisplay{id, uuid, bounds, visible_bounds, refresh_interval}` (`platform.rs:681-717`); `window.refresh_interval(cx)`, `observe_window_display` (`window.rs:3236-3252`), `window.scale_factor()` (`:3506`).
- Wayland: one display per `wl_output`, refresh from the output mode (`crates/gpui_linux/src/linux/wayland/client.rs:1267-1281`, `crates/gpui_linux/src/linux/wayland/display.rs:45-53`), uuid from the output name (`crates/gpui_linux/src/linux/wayland/display.rs:33-39`); **`primary_display()` is always `None`** (`crates/gpui_linux/src/linux/wayland/client.rs:1283-1285`).
- X11: a "display" is an **X screen, not a monitor** (one display spanning all monitors), refresh `None`, uuid all-zero (`crates/gpui_linux/src/linux/x11/display.rs:7-56`, `crates/gpui_linux/src/linux/x11/client.rs:1651-1675`); `on_display_changed` no-op (`crates/gpui_linux/src/linux/x11/window.rs:1759-1761`). Scale: one global factor from `GPUI_X11_SCALE_FACTOR` or `Xft.dpi` (`crates/gpui_linux/src/linux/x11/client.rs:80`, `:2627-2679`).
- Windows: `EnumDisplayMonitors`; work area as `visible_bounds`; refresh from `EnumDisplaySettingsW.dmDisplayFrequency`; uuid v5 of the device name (`crates/gpui_windows/src/display.rs:121-201`, `:155-186`, `:230-236`).

### 2.23 Cursor

`CursorStyle` (21 variants, `platform.rs:3113-3197`); set per hitbox with `window.set_cursor_style(style, &hitbox)` (`window.rs:4582`). Wayland: `wp_cursor_shape_v1` if available else themed cursor surface (`crates/gpui_linux/src/linux/wayland/client.rs:1375-1413`; mapping `crates/gpui_linux/src/linux/wayland.rs:18-42`); X11 Xcursor names (`crates/gpui_linux/src/linux/platform.rs:1376-1402`, `crates/gpui_linux/src/linux/x11/client.rs:1760-1804`); Windows `load_cursor` (`crates/gpui_windows/src/platform.rs:1004-1014`). Hide until mouse moves: `CursorHideMode` + `App::set_cursor_hide_mode`, `is_cursor_visible` (`crates/gpui/src/app.rs:541`, `:1415-1432`); platform impls `crates/gpui_linux/src/linux/wayland/client.rs:828`, `crates/gpui_linux/src/linux/x11/client.rs:2169`, `crates/gpui_windows/src/platform.rs:1016-1035`.

### 2.24 Appearance (dark/light), text scale, keyboard layout

- `cx.window_appearance()` (`crates/gpui/src/app.rs:1959`), `window.appearance()` (`window.rs:3327`), `window.observe_window_appearance` / `cx.observe_window_appearance(window, ..)` (`window.rs:2686`, `crates/gpui/src/app/context.rs:441-453`). `WindowAppearance::{Light, VibrantLight, Dark, VibrantDark}` (`platform.rs:2970-2991`). `cx.set_window_appearance(..)` is a no-op off macOS (`platform.rs:459-467`).
- Linux (W and X): XDG portal Settings `color-scheme` (`PreferDark`→Dark, else Light) with change stream (`crates/gpui_linux/src/linux/xdg_desktop_portal.rs:48-162`, `:225-231`; X11 `crates/gpui_linux/src/linux/x11/client.rs:603-615`); default Light without a portal (`crates/gpui_linux/src/linux/platform.rs:146`).
- Windows: `UISettings` foreground colour (`crates/gpui_windows/src/util.rs:156-166`), refreshed on `WM_SETTINGCHANGE "ImmersiveColorSet"`, which also flips `DWMWA_USE_IMMERSIVE_DARK_MODE` on the native frame (`events.rs:1242-1266`, `util.rs:130-144`).
- Text scale: `cx.text_scale_factor()` / `on_text_scale_factor_change` (`crates/gpui/src/app.rs:1983`, `:1363`) from GNOME `text-scaling-factor` on Wayland only (`crates/gpui_linux/src/linux/xdg_desktop_portal.rs:78-85`; X11 ignores it, `crates/gpui_linux/src/linux/x11/client.rs:628-631`); 1.0 on Windows (default `platform.rs:475-477`).
- Keyboard layout change: `cx.on_keyboard_layout_change` (`crates/gpui/src/app.rs:1352-1360`); Wayland keymap events (`crates/gpui_linux/src/linux/wayland/client.rs:767`, `:2082`, `:2139`), X11 XKB (`crates/gpui_linux/src/linux/x11/client.rs:1108`, `:1146`, `:1613`), Windows `WM_INPUTLANGCHANGE` (`events.rs:157`, `:1268-1279`).
- System bell: `window.play_system_bell()` (`window.rs:7632`): Wayland `xdg_system_bell_v1` (`crates/gpui_linux/src/linux/wayland/window.rs:2270-2280`), X11 `xcb.bell` (`crates/gpui_linux/src/linux/x11/window.rs:2014-2017`), Windows `MessageBeep` (`crates/gpui_windows/src/window.rs:1075-1078`).

### Gaps

- **System tray: none** on any platform. Build it ourselves (StatusNotifierItem/`ksni`-style on Linux, `Shell_NotifyIconW` on Windows), plus taskbar badges (none either).
- **Single instance + `decibell://` deep links: none.** `on_open_urls`/`on_reopen` never fire on Linux/Windows and `register_url_scheme` errors. We need the scheme registration, argv parsing, IPC to the first instance, and an activation path. On Wayland we cannot hand a forwarded `XDG_ACTIVATION_TOKEN` to an existing window.
- **Attention on Wayland: `request_attention` is a no-op**; the only lever is `activate_window()` (with `app_id` set).
- **No hide/unhide-window API**; `show:false` and `focus:false` are honoured only on Windows. Tray-style "hide" = close + reopen with `QuitMode::Explicit`.
- **CSD on Wayland is our job:** with no xdg-decoration (e.g. GNOME), GPUI silently switches to Client and draws nothing; we must draw the title bar, buttons (honour `button_layout`), shadows/insets and resize edges (`window_shadow` example is the Apache reference). X11 CSD needs a compositor. Windows custom title bar works through `WindowControlArea` (snap layouts included).
- Clipboard **image writes are text-only on Wayland and X11**, and reads block the UI thread up to 4 s. Windows writes no `CF_DIB`.
- Drag-out: Wayland only. Drop-in: file paths only.
- Notifications: no dismiss, no sound, a thread per toast on Linux; toasts are silent and in-process on Windows.
- Keyring labels/targets are hard-coded to Zed's (`"zed-github-account"`, `"zed:url="`), and Windows caps blob size.
- No idle-time, lock/unlock, screensaver or display-required inhibition; thermal always Nominal.
- Native popups (`AnchoredPopup`) only on Wayland; `LayerShell` needs wlr-layer-shell; `PopUp` isn't always-on-top on Wayland; `Floating` is plain on Windows.
- Default inactive / low-power 30 fps caps throttle video in unfocused windows unless overridden.
- X11 has no per-monitor displays or refresh rates; Wayland has no primary display.
- Wayland `is_screen_capture_supported()` lies (`true`, then errors).
- `hide_other_apps`/`unhide_other_apps` panic on Windows (`unimplemented!()`).
- No signal or session-end handling on Linux.

## 3. Frame scheduling and idle behaviour

**Summary.** GPUI draws only on demand. A frame is *drawn* (element tree rebuilt, laid out, painted into a new `Scene`) only when the window's invalidator is dirty or a render is forced; it is *presented* when a new scene was drawn or a few other conditions hold. On **Wayland** the frame source parks completely when nothing is dirty — measured 0 renders and 0 main-thread wakeups over 30 s idle (§13). On **X11** a visible window runs a refresh-rate timer forever (280 wakeups/s on a 280 Hz monitor, measured), but it does not draw. On **Windows** a `VSyncProvider` thread calls `DwmFlush()` in a loop and invalidates every window each vblank for as long as the GPU is attached, so the main thread gets a `WM_PAINT` per vblank at idle (it does not draw either). Both of the latter are patchable (§3.7).

### 3.1 From `cx.notify()` to pixels
1. `Context::notify()` → `App::notify(entity_id)` (`crates/gpui/src/app/context.rs:216-218`, `crates/gpui/src/app.rs:3323-3352`). It looks up the windows that rendered this entity last frame (`window_invalidators_by_entity`, filtered by `tracked_entities`) and calls `WindowInvalidator::invalidate_view` on each (`crates/gpui/src/window.rs:175-200`): marks the view in `dirty_views`, sets `dirty = true`, and wakes the platform's frame waker if the window just became dirty. An entity not rendered in any window only queues an `Effect::Notify` for observers (`crates/gpui/src/app.rs:3341-3347`).
2. At the end of the effect cycle `flush_effects` calls `platform_window.schedule_frame()` for every window that is dirty, needs a present, or has next-frame callbacks (`crates/gpui/src/app.rs:2347-2355`).
3. The platform delivers a frame request into the closure installed by `on_request_frame` (`crates/gpui/src/window.rs:1906-2148`). It computes demand (`is_dirty || force_render || next_frame_callbacks || require_presentation || needs_present || high_rate_input`, `:1971-1976`), applies throttling (§3.3), runs `on_next_frame` callbacks (`:2051-2066`), then `draw()` + `present()` if dirty (`:2068-2098`) or only `present()` if a present is owed (`:2099-2109`). If still dirty afterwards it re-schedules (`:2111-2138`).
4. `Window::draw` (`crates/gpui/src/window.rs:3854-4016`) marks dirty views (`invalidate_entities`), renders roots (`draw_roots`), clears the taffy layout engine and calls `text_system().finish_frame()` (`:3941-3943`), swaps `rendered_frame`/`next_frame`, runs focus listeners, records which entities were read this frame (`record_entities_accessed`, `:4018-4028`: this is what makes step 1 target the right windows), and sets `needs_present`.
5. `Window::present` → `platform_window.draw(&scene)` (`:4039-4056`), which on Linux is the wgpu renderer.

Consequences: a `notify()` on a view no window displays costs no frame; any `notify()` on a displayed view redraws the *whole window* (but see view caching, §3.5). Reading another entity in `render` subscribes the window to it implicitly.

### 3.2 Platform frame sources
- **Wayland** (`crates/gpui_linux/src/linux/wayland/window.rs`): a small state machine `FrameLoop` = `Parked | Scheduled | Ticking | RescheduleRequested | AwaitingCallback | RetryScheduled | PresentationFailed | Unconfigured`. `schedule_frame()` from `Parked` pings a calloop `Ping` source (`:1052-1065`); the ping handler calls `frame()` (`:1026-1037`). `draw()` requests a `wl_surface.frame` callback before presenting and moves to `AwaitingCallback` (`:2050-2096`); the callback's `done` event runs one more `frame()` (`:1017-1024`, client side `crates/gpui_linux/src/linux/wayland/client.rs:1707-1722`), which finds nothing dirty and parks (`complete_frame`, `:973-1015`). So each drawn frame costs exactly one extra wake (the compositor's frame callback), then the window is silent. Occluded windows: the compositor withholds frame callbacks, so a dirty-but-hidden window does not spin; a failed present retries on a timer (`client.rs:639-650`).
- **X11** (`crates/gpui_linux/src/linux/x11/client.rs:1973-2092`): while a window is visible (mapped and not `FULLY_OBSCURED`, `:97-103`) a calloop `Timer` fires every display refresh interval (refresh rate read from RandR, 60 Hz fallback, `:2017-2038`) and calls `window.refresh(RequestFrameOptions{require_presentation: false, ..})` (`:2072-2077`, `crates/gpui_linux/src/linux/x11/window.rs:1206-1216`) and `process_x11_events`. Nothing is drawn unless dirty, but **the main thread wakes at the monitor's refresh rate forever** (measured: 280 wakeups/s on a 279.87 Hz monitor, §13). Only unmapping (minimise) or a `FULLY_OBSCURED` `VisibilityNotify` stops it; under a compositing manager / XWayland the server rarely reports full obscuring (*inferred*). Present mode is `Fifo` (`crates/gpui_linux/src/linux/x11/window.rs:782` passes `None`, default `Fifo` at `crates/gpui_wgpu/src/wgpu_renderer.rs:476-479`).
- **Windows** (`crates/gpui_windows/src/platform.rs:432-503`, `crates/gpui_windows/src/vsync.rs:31-58`): one `VSyncProvider` thread started with the DirectX devices (`attach_gpu`, `platform.rs:415-420`) loops on `DwmFlush()` (falls back to `sleep(interval)` when DWM returns early, e.g. monitor off) and calls `RedrawWindow(RDW_INVALIDATE)` on every window each vblank (`platform.rs:490-494`). `WM_PAINT` → `draw_window` → request-frame closure (`crates/gpui_windows/src/events.rs:109,1374-1393`). Not drawn unless dirty, but **two threads wake per vblank at idle**. Stopped only by `detach_gpu` (no windows) or drop (`platform.rs:423-430,1470-1474`).
- **Headless** Linux platform: no frame source (`crates/gpui_linux/src/linux/headless.rs`).

### 3.3 Throttling and presenting without drawing
- `WindowOptions::inactive_frame_interval` (default 33.3 ms ≈ 30 fps), `low_power_frame_interval` (33.3 ms when the power profile is `power-saver`), `thermal_frame_interval` (16.7 ms under serious/critical thermal state) (`crates/gpui/src/platform.rs:2734-2750,2903-2905`). They only cap *demanded* frames (`minimum_frame_interval`, `crates/gpui/src/window.rs:1377-1403`; `FramePacer`, `:1349-1375`); `None` disables each. An inactive window animating something is capped at 30 fps by default.
- **High-rate input keeps presenting**: when ≥ 6 input events that *caused an invalidation* arrive within 100 ms, every frame request for the next 1 s re-presents the last scene even if nothing changed (`InputRateTracker`, `crates/gpui/src/window.rs:1337-1343,1405-1434`; recorded at `:6381-6384`; used at `:1969,2069-2072`). Purpose in Zed: keep display clocks up. For us: after a scroll or drag burst the GPU presents at full refresh rate for one more second. Patch candidate if it shows up in measurements.
- **Scene animations**: an `svg()` with a `TimedTransformation` (`crates/gpui/src/elements/svg.rs:~384-478`: rotation / scale / translate / alpha with easing, `alternate()`, `with_max_fps(f32)`) becomes an animated monochrome sprite. The window then re-presents the *same* scene with the sprite moved (`Scene::advance_animations`, `crates/gpui/src/scene.rs:89-129`; `Window::advance_scene_animations`, `window.rs:3107-3110`) at most at the sprite's `min_frame_interval` — no element tree rebuild. Good for spinners; still one GPU present per tick.

### 3.4 Next-frame callbacks, `request_animation_frame`, `Animation`
- `Window::on_next_frame(FnOnce(&mut Window, &mut App))` queues a callback for the next frame request and pokes the platform (`crates/gpui/src/window.rs:3082-3088`). The callbacks run at the start of that request, before drawing (`:2051-2066`); on Wayland that is when the compositor's frame callback for the previous present fires, so it is a usable "previous frame reached the screen" signal (the hello-world uses it to timestamp the first frame).
- `Window::request_animation_frame()` = `on_next_frame(|_, cx| cx.notify(current_view))` (`:3102-3105`): re-renders the calling view next frame; call it every frame for continuous animation; stop calling it and frames stop.
- `AnimationExt::with_animation(id, Animation, fn)` (`crates/gpui/src/elements/animation.rs`) drives an element's style per frame via `request_animation_frame` until the animation ends (honours `App::reduce_motion`, see doc at `window.rs:3097-3101`). Repeating animations never stop by themselves — remove the element or the animation when it is off screen.
- **Blinking caret without continuous frames**: GPUI has no caret element. Zed's approach (GPL editor code, don't copy) is a timer task that flips a `visible` flag and `notify()`s every ~500 ms, paused while unfocused/typing. With the composer in its own (cached) view each blink redraws the window but re-renders only the composer (§3.5). Alternative: draw the caret as an `svg()` with `TimedTransformation` alpha + `with_max_fps(2.0)` → present-only ticks, no re-render. Either way: 2 frames/s while focused, zero when the window is inactive (check `window.is_window_active()`, `crates/gpui/src/window.rs:3377`).

### 3.5 What is rebuilt per frame, and caching
- Every drawn frame calls `render()` on the root view and on every *non-cached* view in the tree, rebuilds the element tree in a bump arena (`ELEMENT_ARENA`, 1 MiB chunks, `crates/gpui/src/window.rs:342`, `crates/gpui/src/app.rs:1104-1105`, reset per frame via `ArenaClearNeeded`, `window.rs:2086-2089`), runs taffy layout (the layout engine is cleared every frame, `window.rs:3941`), prepaint and paint into a fresh `Scene`.
- `AnyView::cached(StyleRefinement)` / `Entity<V>::cached(..)` (`crates/gpui/src/view.rs:46-48,258-260`) wrap a view so that, when it is not in `dirty_views` and its bounds/content mask/text style are unchanged, the previous frame's prepaint and paint ranges are replayed (`reuse_prepaint` / `reuse_paint`) instead of calling `render` — the scene primitives are copied, not regenerated. Use one cached view per pane (channel list, message list, member list, composer) so a caret blink or typing does not re-render the message list.
- Text shaping is cached across frames by the line layout cache (double-buffered per frame; §5 for details) — unchanged text is not re-shaped. Glyph rasters and SVG rasters live in the sprite atlas across frames (§8).
- Allocation: the arena makes per-element allocation cheap, but every drawn frame still allocates `SmallVec`/`Vec`s for listeners, hitboxes and the new scene; nothing is allocated at idle because nothing runs.

### 3.6 Renderer configuration (Linux, `gpui_wgpu`)
- Instance backends `VULKAN | GL` (`crates/gpui_wgpu/src/wgpu_context.rs:486-494`); every adapter of both is enumerated (`:531`). Ordering: `ZED_DEVICE_ID` override, then the compositor's GPU (from the Wayland dmabuf main device, `crates/gpui_linux/src/linux/platform.rs:1678-1715`), then discrete > integrated > other, then Vulkan before GL (`crates/gpui_wgpu/src/wgpu_context.rs:609-652`). Each candidate is tested by actually configuring the surface (`:511-600`). **wgpu's own env vars (`WGPU_BACKEND`, `WGPU_POWER_PREF`, `WGPU_ADAPTER_NAME`) are not read** — the descriptor is built by hand; the only knob is `ZED_DEVICE_ID=<pci id>` (`:326-339`). Text gamma knobs: `ZED_FONTS_GAMMA` (default 1.8), `ZED_FONTS_GRAYSCALE_ENHANCED_CONTRAST` (1.0), `ZED_FONTS_SUBPIXEL_ENHANCED_CONTRAST` (0.5) (`crates/gpui_wgpu/src/wgpu_renderer.rs:2547-2566`).
- Surface: present mode `Mailbox` on Wayland (`crates/gpui_linux/src/linux/wayland/window.rs:587`), `Fifo` on X11 (default at `crates/gpui_wgpu/src/wgpu_renderer.rs:476-479`); `desired_maximum_frame_latency: 2` (`:480`); premultiplied/opaque alpha chosen per `transparent` (`:424-453`). No MSAA anywhere (`sample_count: 1`, `:707,1586,2342`; atlas `crates/gpui_wgpu/src/wgpu_atlas.rs:212`). Device requested with `MemoryHints::MemoryUsage` and downlevel limits (`crates/gpui_wgpu/src/wgpu_context.rs:455-473`). Instance buffer starts at 2 MiB and grows to ≤ 256 MiB (`crates/gpui_wgpu/src/wgpu_renderer.rs:19,1215-1245`). Atlas textures are 1024×1024 per kind (R8 monochrome, BGRA polychrome/subpixel) and grow by adding textures (`crates/gpui_wgpu/src/wgpu_atlas.rs:183-215`).
- Measured on this machine: the GL adapter (NVIDIA EGL) fails surface configuration on both Wayland and XWayland ("no compatible surface formats"), so **Vulkan is the only working path here**; hiding Vulkan makes the app exit with "No GPU adapter found that can configure the display surface".

### 3.7 Threads GPUI starts (Linux) and what wakes them
Measured thread list of the hello-world (33 threads on Wayland; §13 for counts):
| Thread (comm) | Started by | Wakes on |
|---|---|---|
| main | — | calloop `EventLoop::run(None, ..)` blocks indefinitely (`crates/gpui_linux/src/linux/platform.rs:665-676`); Wayland socket, frame ping, dispatcher ping, timers |
| `Worker-0..N-1` (N = `available_parallelism()`, 20 here) | `LinuxDispatcher::new` (`crates/gpui_linux/src/linux/dispatcher.rs:28-52`) | background tasks; park on a `Condvar` priority queue (`crates/gpui/src/queue.rs:35,88`) |
| `Timer` | same, `dispatcher.rs:54-94` | a calloop loop for `dispatch_after` (all `background_executor().timer()`s: `crates/gpui/src/platform_scheduler.rs:129-150`) |
| `smol-1`, `async-io` | `smol::spawn` of the logind sleep/wake listener (`crates/gpui_linux/src/linux/platform.rs:166-182`) — smol's global executor + async-io reactor | D-Bus traffic only |
| `zbus::Connectio[n]` ×3 | zbus connections: XDG portal settings (appearance, cursor, etc.), logind, power-profiles (`platform.rs:614-635`, `crates/gpui_linux/src/linux/power.rs:31-80`) | D-Bus signals only |
| `Clipboard` (X11 only) | X11 clipboard owner | selection requests |
| driver threads (`[vkps] Update`, `[vkcf]/[vkrt] Analysis`, 3 unnamed) | NVIDIA Vulkan driver | `[vkps] Update` 4 wakeups/s, one unnamed 0.5/s — driver behaviour, not GPUI |
| `VSyncProvider` (Windows) | `crates/gpui_windows/src/platform.rs:444-446` | every vblank |
Realtime threads are spawned only for `Priority::RealtimeAudio` tasks (`crates/gpui/src/executor.rs:140-142`, `dispatcher.rs:144-164`). The worker count is hard-coded to the CPU count (no setter); it respects the process CPU affinity/cgroup quota because it uses `available_parallelism`.

**Patches for idle wakeups (if we need them):** X11 — start the refresh timer only while the window has frame demand and stop it when a request found nothing to do (state already tracked in `RefreshState`, `crates/gpui_linux/src/linux/x11/client.rs:1973-2051`). Windows — park the `VSyncProvider` loop on a condition variable while no window is dirty and wake it from `schedule_frame` (`crates/gpui_windows/src/platform.rs:432-503`). Both are contained in one file each.

## 4. Executors, timers, and talking to our tokio core

### 4.1 The two executors
- `BackgroundExecutor` (`crates/gpui/src/executor.rs:19-23,85-145`): `spawn(impl Future + Send + 'static) -> Task<R>`, `spawn_with_priority(Priority, ..)`; runs on the `Worker-N` pool (§3.7). `Priority::RealtimeAudio` gets its own SCHED_FIFO thread (`:140-142`). `scoped(..)` for borrowing tasks (`:152-165`). `timer(Duration) -> Task<()>` (`:200-205`) is implemented by sending a runnable to the dispatcher's `Timer` thread (`crates/gpui/src/platform_scheduler.rs:129-150`, `crates/gpui_linux/src/linux/dispatcher.rs:135-142`): one calloop timer per call; the awaiting task is then woken on whichever executor it lives on. `now()` (`executor.rs:192-194`) uses the scheduler clock so tests can fake time.
- `ForegroundExecutor` (`executor.rs:27-33,316-437`): `!Send`, runs futures on the main thread: `spawn(impl Future + 'static)`; `spawn_when_idle(timeout, fut)` (`:409-428`, low priority on Linux). `to_send() -> SendForegroundExecutor` (`:318-322`, `:37-49`) is a **`Send + Clone` handle that spawns `Send` futures onto the main thread from any thread** (`crates/scheduler/src/executor.rs:208-240`).
- On Linux, main-thread runnables go into a priority queue plus a calloop `Ping` (an eventfd) (`dispatcher.rs:119-133,167-190`); the event loop drains *all* queued runnables per wake (`:232-287`) and runs each as an idle callback after pending Wayland/X11 events (`crates/gpui_linux/src/linux/platform.rs:184-200`).
- `Task<T>` (`crates/scheduler/src/executor.rs`): a future; dropping it cancels; `.detach()` (`:610,691`) lets it run on. Hold tasks in the view struct to tie their lifetime to the view.

### 4.2 Contexts that cross await points
- `App::spawn(async |cx: &mut AsyncApp| ..)` (`crates/gpui/src/app.rs:2606-2616`), `Context<T>::spawn(async |this: WeakEntity<T>, cx: &mut AsyncApp| ..)` (`crates/gpui/src/app/context.rs:225-233`), `Window::spawn(cx, async |cx: &mut AsyncWindowContext| ..)` (`crates/gpui/src/window.rs:3130-3140`).
- `AsyncApp::update(|app| ..) -> R` (`crates/gpui/src/app/async_context.rs:176-180`) flushes effects after the closure (so `notify()` inside it schedules the frame). `WeakEntity::update(cx, |this, cx| ..)` returns `Result` (entity may be gone). `AsyncWindowContext::update` returns `Result` (window may be closed) (`crates/gpui/src/app/async_context.rs:316`).
- `cx.background_spawn(..)` is the `App`-level shorthand for the background executor.

### 4.3 Delivering events from our tokio/media threads with one wake per burst
GPUI does not run inside tokio, and our core owns its own tokio runtime and media threads. Recommended bridge (all APIs above, no extra crate):
```rust
// UI side, once, in the root view's constructor:
let (tx, mut rx) = futures::channel::mpsc::unbounded::<CoreEvent>();   // tx: Send + Clone → give to the core
self._pump = cx.spawn(async move |this, cx| {
    while let Some(first) = rx.next().await {          // parks the task; no polling
        let mut batch = vec![first];
        while let Ok(Some(ev)) = rx.try_next() { batch.push(ev); }   // drain the burst
        if this.update(cx, |state, cx| { state.apply(batch); cx.notify(); }).is_err() { break; }
    }
});
```
Why this is one wake per burst: a send from a tokio thread wakes the task's waker; `async-task` schedules a runnable only if the task is not already scheduled, and scheduling = queue push + eventfd write (`dispatcher.rs:177-183`); further sends before the main thread runs it are absorbed; the main loop drains every queued runnable per wake. The task then applies the whole batch and calls `notify()` once → one frame (which itself is coalesced per window by the invalidator, §3.1). Use a bounded channel (or a coalescing slot, e.g. "latest volume level") for high-rate media telemetry so a stalled UI thread can't grow memory. Equivalent alternative: keep a `SendForegroundExecutor` (`cx.foreground_executor().to_send()`) in the core and `spawn` a closure-future per event — simpler but one runnable per event.
Don't: call `cx.update` from another thread (contexts are `!Send`); block the main thread on tokio (`block_on`) — use `Tokio::spawn`-style bridging instead.

### 4.4 `gpui_tokio`
`crates/gpui_tokio/src/gpui_tokio.rs` (100 lines, Apache-2.0, depends only on `gpui`, `gpui_util`, `anyhow`, `tokio` with `rt-multi-thread`, `crates/gpui_tokio/Cargo.toml:6,16-20`). `init(cx)` builds its own 2-worker multi-thread runtime (`:12-25`); `init_from_handle(cx, Handle)` adopts ours (`:28-33`); `Tokio::spawn(cx, fut) -> Task<Result<R, JoinError>>` spawns on tokio and awaits the `JoinHandle` from a GPUI *background* task, aborting the tokio task if the GPUI task is dropped (`:53-74`); `Tokio::handle(cx)` (`:97-99`). It is a convenience for request/response calls from UI code; it costs one background-worker wake per call on completion. For our event stream use §4.3. We can depend on it (Apache-2.0) or copy its 30 relevant lines.

### 4.5 Timers in the UI
- `cx.background_executor().timer(d).await` inside a foreground task (`executor.rs:200-205`): the Timer thread fires, then the main thread is woken to resume. Use for debounce, caret blink, "typing…" expiry.
- Periodic work keeps waking the process: every repeating timer is a wake source. Stop timers when the window is inactive/hidden (`observe_window_activation`, `Window::is_window_active`).

## 5. Text

Verified at zed `a3ce8d5e`; `path:line` is relative to the zed root. Dependency code is cited at the versions pinned in zed's `Cargo.lock`, read from the local cargo registry `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/` as `[cosmic-text-0.19.0]/…`, `[fontdb-0.23.0]/…`, `[swash-0.2.10]/…`, `[icu_segmenter-2.3.0]/…`, and `[zed-font-kit]/…` (= `~/.cargo/git/checkouts/font-kit-2de8721bb3347bf4/94b0f28`). DirectWrite behaviour that can't be read from source is marked *(MS docs, not verified here)*.

### 5.1 Text stacks and the platform trait

- **Trait** `PlatformTextSystem: Send + Sync` (`crates/gpui/src/platform.rs:1638-1673`):
  - `add_fonts(&self, Vec<Cow<'static, [u8]>>) -> Result<()>`; `set_missing_glyph_sink(Option<Arc<dyn MissingGlyphSink>>)` (default no-op); `all_font_names() -> Vec<String>`; `font_id(&Font) -> Result<FontId>`; `prewarm_fonts(&[FontId])` (default no-op)
  - `font_metrics(FontId) -> FontMetrics`; `typographic_bounds(FontId, GlyphId) -> Result<Bounds<f32>>`; `advance(FontId, GlyphId) -> Result<Size<f32>>`; `glyph_for_char(FontId, char) -> Option<GlyphId>`
  - `glyph_raster_bounds(&RenderGlyphParams) -> Result<Bounds<DevicePixels>>`; `rasterize_glyph(&RenderGlyphParams, Bounds<DevicePixels>) -> Result<(Size<DevicePixels>, Vec<u8>)>`
  - `layout_line(&self, text: &str, font_size: Pixels, runs: &[FontRun]) -> LineLayout`: one line, no wrapping
  - `recommended_rendering_mode(FontId, Pixels) -> TextRenderingMode`; `glyph_dilation_for_color(Hsla) -> u8` (default 0)
- Two wrappers: `TextSystem` (app-wide: font resolution, metric and raster-bound caches; `crates/gpui/src/text_system.rs:231-242`) and `WindowTextSystem` (per window; adds the `LineLayoutCache`; `text_system.rs:635-653`). The app builds `TextSystem::new(platform.text_system())` once (`crates/gpui/src/app.rs:1018`).
- **Linux**: `gpui_wgpu::CosmicTextSystem` (`crates/gpui_wgpu/src/cosmic_text_system.rs`). `LinuxCommon::new` always constructs it as `CosmicTextSystem::new("IBM Plex Sans")` (`crates/gpui_linux/src/linux/platform.rs:131-134`).
  - **cosmic-text 0.19.0** (`crates/gpui_wgpu/Cargo.toml:26`) with its default features `std, swash, fontconfig` (`[cosmic-text-0.19.0]/Cargo.toml` `[features]`). Under it: fontdb 0.23 (font database), harfrust 0.5 (Rust HarfBuzz port, shaping), skrifa 0.40 (metrics), unicode-bidi, unicode-linebreak (all in `Cargo.lock`).
  - Shaping: `ShapeLine::new(.., Shaping::Advanced, ..)`, then `layout_to_buffer(.., Wrap::None, Ellipsize::None, .., Hinting::Disabled)`. GPUI does its own wrapping (`crates/gpui_wgpu/src/cosmic_text_system.rs:747-765`).
  - Rasterisation: GPUI calls **swash 0.2.10** (`Cargo.toml:933`) directly through its own `ScaleContext`, not cosmic's `SwashCache` (`crates/gpui_wgpu/src/cosmic_text_system.rs:512-556`).
  - font-kit: `gpui_linux` enables `gpui_wgpu/font-kit` (`crates/gpui_linux/Cargo.toml:64`). It is used only for `font_kit::matching::find_best_match`, a CSS-style face matcher (`crates/gpui_wgpu/src/cosmic_text_system.rs:968-992`).
  - fontconfig: fontdb reads the fontconfig XML config with the pure-Rust `fontconfig-parser` crate to find font directories (`[fontdb-0.23.0]/src/lib.rs:503-583`). libfontconfig is never called at runtime. But zed-font-kit has unconditional Linux dependencies on `freetype-sys` and `yeslogic-fontconfig-sys` (`[zed-font-kit]/Cargo.toml:59-63`), so both still get built and linked.
- **Windows**: `DirectWriteTextSystem` (`crates/gpui_windows/src/direct_write.rs:36-121`) uses `IDWriteFactory5`. Embedded fonts go through an `IDWriteInMemoryFontFileLoader` and an `IDWriteFontSetBuilder1`; system fonts come from `GetSystemFontCollection` (`:96-102`). It needs Windows 10 1703 or later (comment at `:73-75`).
  - Shaping: one `IDWriteTextLayout` per line, drawn through a custom `IDWriteTextRenderer` that captures the glyph runs (`:479-601`, `:1290-1455`).
  - Rasterisation: `IDWriteGlyphRunAnalysis` alpha textures (`:687-710`, `:851-914`).
  - Faces are `IDWriteFontFace3` (`:30`). No axis API is used: searched `Axis`, `AXIS` and `FontFace5` in `direct_write.rs`, not found.

### 5.2 Fonts: loading, naming, resolution, startup cost

**Types and resolution**
- `Font { family: SharedString, features: FontFeatures, fallbacks: Option<FontFallbacks>, weight: FontWeight, style: FontStyle }` (`text_system.rs:1148-1166`). Builders: `font("Inter")` (`:1175-1183`), `.bold()`, `.italic()` (`:1185-1197`). It has **no stretch/width, no optical size and no variation axes**.
- `FontId(pub usize)` is an index into the platform's vector of loaded faces (`text_system.rs:41-44`; Linux `loaded_fonts` `crates/gpui_wgpu/src/cosmic_text_system.rs:55`; Windows `fonts` `crates/gpui_windows/src/direct_write.rs:64`).
- `TextSystem::resolve_font(&Font) -> FontId` (`text_system.rs:352-359`):
  - Looks in an `FxHashMap<Font, CachedFont>` cache, then calls `platform.font_id`.
  - If that fails, it tries a hard-coded fallback stack: `.ZedMono, .ZedSans, Helvetica, Segoe UI, Ubuntu, Adwaita Sans, Cantarell, Noto Sans, DejaVu Sans, Arial` (`:262-274`).
  - It **panics** if none of them resolve (`:620-629`).
- Virtual family names (`text_system.rs:1277-1287`):
  - `.SystemUIFont` → the platform system font. On Linux that is the string passed at `crates/gpui_linux/src/linux/platform.rs:132` (`"IBM Plex Sans"`). On Windows it is the `SPI_GETICONTITLELOGFONT` face, else "Segoe UI" (`crates/gpui_windows/src/direct_write.rs:1730-1752`, used at `:366-371`).
  - `.ZedSans` → "IBM Plex Sans"; `.ZedMono` → "Lilex".
  - `TextStyle::default()` uses `.SystemUIFont` (`crates/gpui/src/style.rs:490`).
- `TextSystem::add_fonts(Vec<Cow<'static,[u8]>>)` (`text_system.rs:302-308`):
  - Clears the Font→FontId cache and bumps `font_generation`, which empties every window's line-layout cache (`crates/gpui/src/text_system/line_layout.rs:756-771`). Add all fonts at startup, before the first frame.
  - Related: `all_font_names()` (`:291-296`) and `prewarm_fonts(&[Font])` (`:361-375`; its doc says to run it on a background executor).

**Linux resolution** (`CosmicTextSystem::font_id` `crates/gpui_wgpu/src/cosmic_text_system.rs:190-209`; `load_family` `:339-446`)
- **Family match**: exact string equality against every family name fontdb recorded for a face: the typographic family (name ID 16), else the family (ID 1), in all languages (`[fontdb-0.23.0]/src/lib.rs:1056-1066`; `crates/gpui_wgpu/src/cosmic_text_system.rs:385-393`).
- **One FontId per face**: per (fontdb face, family, features, fallbacks) (`LoadedFontKey` `:43-47`).
- **Weight/style choice**: font-kit's CSS matcher picks among the family's faces. The query stretch is always `NORMAL` (`:1166-1177`; `[zed-font-kit]/src/matching.rs:21-156`).
- **Ties**: they go to the first candidate (`matching.rs:152-155`). System fonts are loaded into the database before `add_fonts` ones, so a system-installed family with the same name and weight wins over the embedded copy. cosmic also re-resolves faces by family/weight/style on its own and GPUI adopts the face it shaped with (`:791-808`). Embedded families should not share a name with installed fonts.
- **The `'m'` rule**: a face whose cmap lacks `'m'` is never accepted as an explicit or primary font (`:414-432`), nor as a `FontFallbacks` entry (`:366-372`). This matters for emoji and spacer fonts (5.6).
- **`add_fonts` storage**: each font becomes `fontdb::Source::Binary(Arc::new(bytes))` (`:329-336`), so a `Cow::Borrowed(&'static [u8])` (e.g. from `include_bytes!`) is not copied.

**Linux startup scan (RSS/CPU)**
- **Eager scan**: `CosmicTextSystem::new` → `FontSystem::new()` (`crates/gpui_wgpu/src/cosmic_text_system.rs:131-145`) → `db.load_system_fonts()` (`[cosmic-text-0.19.0]/src/font/system.rs:191-210, 434-450`). Its doc says this "can take up to a second" in release builds (`system.rs:184-190`).
- **What it does**: fontdb walks every fontconfig directory. For each file it mmaps it, parses `name`/`OS/2`/`post`, stores a `FaceInfo` (names, PostScript name, style, weight, stretch, `Source::File(path)`) and unmaps the file (`[fontdb-0.23.0]/src/lib.rs:266-273, 503-583, 811-857, 1034-1054`).
- **What it costs**: the kept data is only the vector of `FaceInfo` records; the mmaps are transient. For scale, this machine has 1152 faces in 1050 files, about 900 MB (`fc-list`).
- **Repeated hidden costs while system fonts are loaded**:
  - `FontSystem::get_font_matches` runs per distinct family/weight/style/stretch (256-entry cache, cleared when full). It builds a `FontMatchKey` for **every face in the database**. For each face whose weight differs from the request, it mmaps the file again to check its `wght` axis range (`system.rs:33-44, 359-418`; fontdb `with_face_data` `[fontdb-0.23.0]/src/lib.rs:721-727, 911-917`).
  - The missing-glyph fallback (`[cosmic-text-0.19.0]/src/shape.rs:326-370`) tries three tiers in order (`[cosmic-text-0.19.0]/src/font/fallback/mod.rs:425-480`):
    1. Per-script family lists.
    2. A common list (`Noto Sans`, `DejaVu Sans`, …, `Noto Color Emoji`).
    3. **Every face in the database.**
  - Each face it tries is loaded with `get_font`: `make_shared_face_data` keeps the file mmapped, and `Font::new` builds harfrust data. The result is cached forever in `font_cache` (`system.rs:140, 302-321`). From reading the code (not measured), one grapheme that no installed font covers can make it map and parse every system font once.
- **Avoidable with a one-line patch**:
  - `CosmicTextSystem::new_without_system_fonts(system_font_fallback: &str)` exists (`crates/gpui_wgpu/src/cosmic_text_system.rs:147-164`): an empty `fontdb::Database` and locale "en-US". The crate's own tests use it (`:1221-1239`).
  - It is not reachable through public configuration, because the constructor call is hard-coded at `crates/gpui_linux/src/linux/platform.rs:132`. Patch that line in the vendored crate.
  - Pass our UI family as `system_font_fallback` so `.SystemUIFont` resolves. Otherwise any element still on `TextStyle::default()` goes through the fallback stack and panics.
  - Wrapping the `Platform` via `Application::with_platform(Rc<dyn Platform>)` (`crates/gpui/src/app.rs:242-248`) can override `text_system()`, but the scan has already happened inside `LinuxCommon::new`.
- **Lazy fallback without the scan**:
  - With no system fonts there is no CJK, Arabic or emoji fallback unless we ship those fonts or add them later.
  - `cx.on_missing_glyphs(|glyphs: &[MissingGlyph], cx: &mut App| ..) -> Subscription` (`crates/gpui/src/app.rs:2663-2687`) delivers `MissingGlyph { grapheme, font_class }` (`text_system.rs:76-101`). This is Linux only: DirectWrite doesn't implement the sink.
  - From that callback, find a covering system font file and pass it to `add_fonts`. mmap the file and `Box::leak` the map to get a `Cow::Borrowed`, so its pages stay file-backed.
- **A second system font database**:
  - GPUI's SVG renderer keeps its own `LazyLock` usvg fontdb built with `load_system_fonts()` (`crates/gpui/src/svg_renderer.rs:141-168`).
  - It is built only when an SVG containing `<text>` is parsed (it lives in the font-resolver callback). Icon and Twemoji SVGs have no `<text>`, so they don't trigger it.

**Windows**
- **No in-process scan**: the system collection comes from DirectWrite's shared font cache (`crates/gpui_windows/src/direct_write.rs:96-102`).
- **`add_fonts`**: calls `CreateInMemoryFontFileReference(factory, ptr, len, None)` and `builder.AddFontFile`, then rebuilds the custom collection from the builder (`:248-284`).
  - With a null owner, DirectWrite copies the bytes *(MS docs, not verified here)*. The `Cow::Owned` branch drops its buffer right after the call (`:266-276`), which is only sound if DirectWrite copies.
  - So expect each embedded font to cost its full size in RAM on Windows.
- **Lookup**: the custom collection first, then the system one (`:207-218`), via `IDWriteFontSet::GetMatchingFonts(family, weight, DWRITE_FONT_STRETCH_NORMAL, style)` (`:372-383`). On a miss it refreshes the system collection once (`:227-242`).

### 5.3 Weights, widths, variable-font axes

- **What GPUI can express**:
  - `FontWeight(pub f32)` (`text_system.rs:983-985`) with constants `THIN`..`BLACK` (`:1014-1046`), so `FontWeight(450.0)` is valid.
  - `FontStyle { Normal, Italic, Oblique }` (`:1066-1075`).
  - There is no stretch, axis list or opsz in `Font` (`:1148-1166`), `TextRun`, `TextStyle` (`crates/gpui/src/style.rs:438-483`) or `HighlightStyle` (`crates/gpui/src/style.rs:580`).
- **Linux, a variable TTF passed to `add_fonts`**:
  - **One face**: fontdb stores one `FaceInfo` per face index, with the OS/2 `usWeightClass`; it does not enumerate named instances (`[fontdb-0.23.0]/src/lib.rs:1034-1054`). A variable Inter is therefore one face, with the weight of its default instance (e.g. 400).
  - **All weights resolve to it**: every `Font.weight` resolves to that same FontId, the only candidate (`crates/gpui_wgpu/src/cosmic_text_system.rs:206`).
  - **Shaping ignores the requested weight**: the attrs carry the face's weight (`FontMatchProperties.weight = face.weight`, `:302-309`, used by `attributes()` `:83-91`). The harfrust instance location sets only `wght`, to that weight (`[cosmic-text-0.19.0]/src/font/mod.rs:140-144, 200-205`). GPUI also loads faces with `get_font(id, Weight::NORMAL)` (`crates/gpui_wgpu/src/cosmic_text_system.rs:409-412, 574-577`).
  - **Rasterisation uses the default instance**: the swash scaler is built without `variations`/`normalized_coords` (`:525-530`). cosmic-text's own renderer does apply `wght` (`[cosmic-text-0.19.0]/src/swash.rs:28-41`), but GPUI doesn't use it.
  - **No synthetic bold or oblique**: swash embolden/skew are never called (`:542-550`).
  - **Result**: medium, bold or 450 from one variable file all render at the default weight on Linux. `opsz` and `wdth` are never set, so they stay at their defaults.
- **Windows**:
  - **Weight conversion**: `font_weight_to_dwrite` truncates to `DWRITE_FONT_WEIGHT(i32)` (`crates/gpui_windows/src/direct_write.rs:1581-1583`).
  - **Face selection**: `GetMatchingFonts(weight, STRETCH_NORMAL, style)`, taking the first face that creates successfully (`:374-408`).
  - **Layout**: the text format is created with that face's `GetWeight()` and `STRETCH_NORMAL` (`:503-514`).
  - **Variable fonts**: `IDWriteFontSetBuilder1::AddFontFile` adds each named instance of a variable font as its own font-set entry, so a weight snaps to the nearest named instance with no interpolation *(MS docs, not verified here)*.
  - **No axis APIs**: GPUI never calls them (`SetFontAxisValues`, `SetAutomaticFontAxes`, `IDWriteFontFace5`). opsz is never applied automatically and wdth can't be selected.
- **Width, both platforms**: the stretch query is always NORMAL (`crates/gpui_wgpu/src/cosmic_text_system.rs:1175`; `crates/gpui_windows/src/direct_write.rs:379, 510`). A width variant is only reachable as a separately named family.
- **Workable plan without patching GPUI**:
  - Instance the variable files offline (fontTools `varLib.instancer`) into static TTFs: one file per weight we use (e.g. 400/500/600/700/800 for both Inter and Mona Sans).
  - Give the width and opsz cuts their own family names (e.g. "Mona Sans Wide", an "Inter Display"-style opsz cut), and give the weights within a family distinct OS/2 weight classes.
  - Both platforms then select by `FontWeight` through ordinary face matching. A weight like 450 still snaps to the nearest file.
- **Patch route** (vendored `gpui_wgpu`, Apache-2.0):
  - Linux: put the requested weight (and any axes) into `LoadedFontKey`. Pass it to `get_font(id, weight)` and to `FontMatchProperties.weight`. Add `scaler.normalized_coords(..)` in `render_glyph_image`, mirroring `[cosmic-text-0.19.0]/src/swash.rs:28-41`.
  - Windows is a larger job: `IDWriteFontFaceReference1` or `IDWriteTextLayout4::SetFontAxisValues` for layout, plus an axis-aware `IDWriteFontFace5` for rasterisation.

### 5.4 Font features and fallback

- **Features**: `FontFeatures(pub Arc<Vec<(String, u32)>>)` (`crates/gpui/src/text_system/font_features.rs:7-17`). Example: `FontFeatures(Arc::new(vec![("tnum".into(),1), ("ss01".into(),1), ("cv11".into(),1)]))`.
  - `FontFeatures::disable_ligatures()` turns off only `calt` (`:20-22`).
  - Set them per element with `Styled::font_features` (`crates/gpui/src/styled.rs:714-717`) or `Styled::font(Font)` (`:720-737`), or per run through `TextRun.font.features`.
- **Features on Linux**:
  - Every tag must be exactly 4 bytes. Otherwise `load_family` errors (`crates/gpui_wgpu/src/cosmic_text_system.rs:395, 1150-1164`) and the font silently falls back to the fallback stack.
  - Features are part of the FontId key (`:26-31`) and reach the shaper through cosmic's `Attrs::font_features` (`:90`).
  - Granularity: cosmic splits shaping runs only when family, weight, style or stretch change (`Attrs::compatible`, `[cosmic-text-0.19.0]/src/attrs.rs:413-418`; `shape.rs:825-870`). It takes the features from the first span of each run (`shape.rs:155-160`). So per-range features work at word granularity: a timestamp that is its own word works, one letter inside a word does not.
- **Features on Windows**:
  - One `IDWriteTypography` per FontInfo, applied with `SetTypography` on each run (`crates/gpui_windows/src/direct_write.rs:344-351, 532, 570`).
  - `liga`, `clig` and `calt` are always added with value 1 unless you set them to 0 (`:1628-1665`).
  - Tags must be 4 bytes: `make_open_type_tag` indexes bytes 0–3 (`:1677-1681`).
- **Fallback list type**: `FontFallbacks(pub Arc<Vec<String>>)`, built with `FontFallbacks::from_fonts(Vec<String>)` (`crates/gpui/src/text_system/font_fallbacks.rs:8-21`); set through `TextStyle.font_fallbacks` or `Styled::font`.
- **Fallback on Linux**:
  - The user's chain is resolved when the family loads. Each entry goes through `load_family`; missing entries and entries without `'m'` are dropped (`crates/gpui_wgpu/src/cosmic_text_system.rs:354-383`).
  - At layout time, each grapheme at or above U+0080 that the primary font lacks goes to the first chain font whose cmap covers its first character (`:715-743, 1054-1142`).
  - cosmic's own fallback runs after that (`[cosmic-text-0.19.0]/src/font/fallback/unix.rs:30-50, 56-90`; `fallback/mod.rs:425-480`): per-script lists (e.g. Noto Sans CJK chosen by locale), then a common list ending with `Noto Color Emoji`, then any face. Emoji and CJK fallback therefore depend on the Noto families being installed.
- **Fallback on Windows**:
  - An `IDWriteFontFallback` is built from the user's list plus `GetSystemFontFallback()` (`crates/gpui_windows/src/direct_write.rs:286-342`).
  - The list's families are looked up **only in the system collection** (`:297-307`). Fonts loaded with `add_fonts` can't be fallback entries on Windows; give them explicit runs.
  - Without `fallbacks`, DirectWrite's system fallback applies.
- **Missing-glyph reports (Linux)**: glyph id 0 from cosmic is reported to the sink once `on_missing_glyphs` is registered (`crates/gpui_wgpu/src/cosmic_text_system.rs:778-788, 839-841`).

### 5.5 Size, positioning, antialiasing, line height, decorations

- **Fractional sizes**: `TextStyle.font_size: AbsoluteLength` (px or rems) becomes an unrounded `Pixels(f32)` (`crates/gpui/src/elements/text.rs:834`).
  - Glyphs are rasterised at `font_size * scale_factor` (Linux `crates/gpui_wgpu/src/cosmic_text_system.rs:528`; Windows `crates/gpui_windows/src/direct_write.rs:643`).
  - Each distinct size is its own atlas entry: `RenderGlyphParams` hashes `font_size` (`text_system.rs:1121-1145`).
- **Subpixel positioning**: `SUBPIXEL_VARIANTS_X = 4`, `SUBPIXEL_VARIANTS_Y = 1` (`text_system.rs:50-54`).
  - `paint_glyph` quantises x to ¼ device pixel and snaps the baseline to a whole device pixel (`crates/gpui/src/window.rs:5329-5339`).
  - Emoji snap to whole pixels, with no subpixel variants (`window.rs:5434-5446`).
- **Antialiasing mode**: `TextRenderingMode { PlatformDefault, Subpixel, Grayscale }` (`platform.rs:3018-3028`), set app-wide with `cx.set_text_rendering_mode(mode)` (`crates/gpui/src/app.rs:2006`).
  - LCD rendering is used only when the platform window supports it **and** the window background is `Opaque` (`window.rs:5396-5415`).
  - Linux: the text system always recommends Subpixel (`crates/gpui_wgpu/src/cosmic_text_system.rs:277-283`). The window supports it only with wgpu dual-source blending (`crates/gpui_linux/src/linux/wayland/window.rs:1957-1965`; X11 `crates/gpui_linux/src/linux/x11/window.rs:1666`).
  - Linux BGR detection: `Xft.rgba` on X11 (`crates/gpui_linux/src/linux/x11/client.rs:1714-1716`), the output's subpixel layout on Wayland (`crates/gpui_linux/src/linux/wayland/window.rs:657-663`). Fontconfig's hinting and rgba settings are not read.
  - Windows: Subpixel when `SPI_GETFONTSMOOTHINGTYPE` is ClearType (`crates/gpui_windows/src/direct_write.rs:188-198, 1713-1728`).
  - Memory: LCD glyph tiles are 4 bytes per pixel, grayscale tiles 1 byte (`crates/gpui_wgpu/src/cosmic_text_system.rs:492-508`; `crates/gpui_windows/src/direct_write.rs:858-913`). Grayscale quarters the glyph-atlas memory.
- **Gamma and contrast**: there is no public API for either.
  - Linux reads three environment variables once per renderer (`crates/gpui_wgpu/src/wgpu_renderer.rs:2537-2572`):

    | Variable | Default |
    |---|---|
    | `ZED_FONTS_GAMMA` | 1.8 (clamped to 1.0–2.2) |
    | `ZED_FONTS_GRAYSCALE_ENHANCED_CONTRAST` | 1.0 |
    | `ZED_FONTS_SUBPIXEL_ENHANCED_CONTRAST` | 0.5 |

  - Windows takes gamma, enhanced contrast and BGR from the system `IDWriteRenderingParams1` (`crates/gpui_windows/src/directx_renderer.rs:886-899`).
- **Hinting**:
  - Linux shapes unhinted (`Hinting::Disabled`, `crates/gpui_wgpu/src/cosmic_text_system.rs:764`) but rasterises hinted with swash `.hint(true)` (`:529`). Its mode is skrifa `HintingMode::Smooth { lcd_subpixel: Horizontal }`, i.e. vertical-only, light-style hinting (`[swash-0.2.10]/src/scale/hinting_cache.rs:27-28`).
  - Windows uses `GetRecommendedRenderingMode` with the recommended grid fit at 96 dpi; `OUTLINE` is mapped to `NATURAL_SYMMETRIC` (`crates/gpui_windows/src/direct_write.rs:658-685`). Measuring mode is natural.
- **Line height**: `TextStyle.line_height: DefiniteLength`. It is either absolute (`px(20.)`, `rems(1.375)`) or relative to the font size (`relative(1.375)`).
  - Default: `phi()` = 1.618 (`crates/gpui/src/style.rs:454, 494`; `crates/gpui/src/geometry.rs:3821-3823`).
  - Set with `Styled::line_height` (`crates/gpui/src/styled.rs:740-743`).
  - Text elements pixel-snap it (`text.rs:835-839`), and one value applies to every row of an element.
- **Letter and word spacing**: **none**. Searched `letter_spacing`, `tracking` and `word_spacing` in `crates/gpui/src`, `crates/gpui_wgpu/src` and `crates/gpui_windows/src`: not found.
- **Alignment**: `TextAlign { Left, Center, Right }` (`crates/gpui/src/style.rs:421-433`); there is no justify.
- **Decorations**: per `TextRun` (`text_system.rs:1084-1098`).
  - `underline: Option<UnderlineStyle { thickness, color, wavy }>` (`crates/gpui/src/style.rs:828-837`).
  - `strikethrough: Option<StrikethroughStyle { thickness, color }>` (`crates/gpui/src/style.rs:843-849`).
  - `background_color: Option<Hsla>`: painted as quads `line_height` tall across the run (`crates/gpui/src/text_system/line.rs:798-938`).
  - Positions are heuristics, not the font's `post`/`OS/2` metrics. Underline y = `(line_height−asc−desc)/2 + asc + 0.618·desc` (`text_system.rs:56-63`); strikethrough y = `(asc/2 + baseline)/2` (`crates/gpui/src/text_system/line.rs:675-680`).
  - A wavy underline is a band 3× the thickness (`window.rs:3625-3629`).

### 5.6 Emoji and inline images

**Colour glyph formats**

| | COLR v0 | COLR v1 | CBDT/CBLC | sbix | SVG-in-OT |
|---|---|---|---|---|---|
| Linux (swash) | yes: `Source::ColorOutline(0)`; swash reads only v0 base/layer records (`[swash-0.2.10]/src/scale/color.rs:24-51`) | no | yes: `ColorBitmap(StrikeWith::BestFit)` (`[swash-0.2.10]/src/strike.rs:33-44`) | yes | no |
| Windows (DirectWrite) | yes: `TranslateColorGlyphRun` asked for `DWRITE_GLYPH_IMAGE_FORMATS_COLR` only (`crates/gpui_windows/src/direct_write.rs:712-768`) | no (paint-tree format never requested) | detected but drawn as a black silhouette | same | same (`:834-843`) |

- **Linux decides colour per font**, by PostScript name. `is_emoji` is set only when the face's PostScript name is exactly `NotoColorEmoji` (`crates/gpui_wgpu/src/cosmic_text_system.rs:809, 1204-1207`).
  - Any other colour font, Twemoji COLR included, is rasterised as a monochrome mask tinted with the text colour (`:532-540`).
  - Fix: a one-line patch to `check_is_known_emoji_font`.
- **Windows decides colour per glyph**, using `IsColorFont()` and `is_color_glyph` (`crates/gpui_windows/src/direct_write.rs:1392, 1436-1437, 1756-1790`). It works for any COLR font.
- **Painting**: emoji glyphs are painted as `PolychromeSprite` at a whole-pixel origin (`window.rs:5425-5474`).

**Inline elements in text: not supported.**
- `StyledText` and `TextLayout` take only a string plus `TextRun`s (`text.rs:367-373, 502-515`).
- On Windows, DirectWrite's `DrawInlineObject` is implemented as `E_NOTIMPL` (`crates/gpui_windows/src/direct_write.rs:1485-1499`).
- `LineFragment::Element { width, len_utf8 }` exists, but only for `LineWrapper::wrap_line`, which `StyledText` doesn't use (`crates/gpui/src/text_system/line_wrapper.rs:683-731`).

**Approach 1: Twemoji as a COLRv0 font** (preferred for Unicode emoji)
- **What it gets right**: real advances, ZWJ and skin-tone ligatures, UAX #14 breaks around emoji, and selection by byte index.
- **How**: segment emoji yourself and give those byte ranges a `TextRun` whose `font.family` is the emoji font. Fallback won't select it on either platform:
  - Windows won't accept embedded fonts as `FontFallbacks`.
  - On Linux, with system fonts loaded, cosmic's fallback finds Noto Color Emoji first.
- **Linux needs two changes**:
  1. Patch `check_is_known_emoji_font`, or rename the font's PostScript name to `NotoColorEmoji`.
  2. Give the font an `'m'` glyph, or patch the `'m'` rule (`crates/gpui_wgpu/src/cosmic_text_system.rs:428-432`), so it can be used as an explicit family.
- **Size**: inline emoji draw at the font size, while Discord uses about 1.375em. A custom build (e.g. nanoemoji from the Twemoji SVGs) can scale the glyphs and advances. Keep its hhea/OS/2 ascent+descent no larger than the text font's, because a line's ascent and descent are the maximum over every font used in it (5.9).
- **Source**: a COLRv0 Twemoji build exists (Mozilla's "Twemoji Mozilla") *(not verified here)*.

**Approach 2: placeholder run plus overlay** (needed anyway for custom server emoji images)
- **Spacer font**: ship a tiny spacer font that has an `'m'` glyph and maps U+FFFC to an empty glyph with the advance you want. U+FFFC is UAX #14 class CB, which allows a break on both sides, like an atomic inline. Express the advance in em of the paragraph's font size, because runs have no size of their own.
- **Text**: emit one U+FFFC per image, with a `TextRun` in the spacer family. Keep a side table mapping byte range → image, for copy and selection text.
- **Finding each image's position after prepaint**: don't use `TextLayout::position_for_index` directly. At a soft-wrap boundary it reports the **end of the previous row**: `WrappedLineLayout::position_for_index` treats `index == boundary` as belonging to the earlier row (`crates/gpui/src/text_system/line_layout.rs:447-474`). Instead:
  1. Find the glyph with that `index` in `line.unwrapped_layout.runs`.
  2. Find its row by comparing its (run_ix, glyph_ix) against `wrap_boundaries`.
  3. Take x = glyph.position.x minus the x of that row's boundary glyph.
  4. Take y = row × line_height, offset from `TextLayout::bounds()` (`text.rs:1031-1033`).
- **Painting**: call `window.paint_image(bounds, image_bounds, corner_radii, Arc<RenderImage>, frame_index, grayscale)` (`window.rs:5574-5581`). Call it from a sibling `canvas(prepaint, paint)` (`crates/gpui/src/elements/canvas.rs:10-13`) that paints after the text, or from a custom element that wraps `StyledText`. Its `TextLayout` is an `Rc` handle you can clone (`text.rs:387-390, 580-582`).

**Approach 3: emoji-only ("jumbo") messages**
- **Use plain `img()` elements**: no text flow is involved.
- **SVG handling**: `img()` of an SVG rasterises full-colour RGBA once through resvg, at intrinsic size × `SMOOTH_SVG_SCALE_FACTOR` (2.0) (`platform.rs:3603-3607`; `crates/gpui/src/svg_renderer.rs:84, 253-287`).
- **Not `svg()`**: `svg()` / `window.paint_svg` produces a monochrome alpha mask only (`window.rs:5482-5560`).

### 5.7 Text types, hit-testing, highlights, truncation, wrapping, bidi

**Data types**
- `TextRun { len (UTF-8 bytes), font: Font, color, background_color, underline, strikethrough }` (`text_system.rs:1084-1098`): a styled input span. It has **no font size**; the size is set once per shaping call.
- `FontRun { len, font_id }` (`crates/gpui/src/text_system/line_layout.rs:805-810`): what the platform receives. TextRuns are merged into FontRuns (`text_system.rs:882-916` for `layout_line`, `:742-793` for `shape_text`).
- `LineLayout { font_size, width, ascent, descent, runs: Vec<ShapedRun>, len }`, `ShapedRun { font_id, glyphs }`, `ShapedGlyph { id, position: Point<Pixels>, index (UTF-8 byte), is_emoji }` (`crates/gpui/src/text_system/line_layout.rs:20-58`): one shaped line before wrapping.
- `WrappedLineLayout { unwrapped_layout: Arc<LineLayout>, wrap_boundaries: SmallVec<[WrapBoundary;1]>, wrap_width }` and `WrapBoundary { run_ix, glyph_ix }` (`:297-315`).
- `ShapedLine` (`crates/gpui/src/text_system/line.rs:41-50`): `Arc<LineLayout>` + text + decoration runs; one visual row.
- `WrappedLine` (`crates/gpui/src/text_system/line.rs:446-455`): `Arc<WrappedLineLayout>` + text + decorations; one '\n'-separated line, possibly several rows.
- **Painting shaped text**:
  - Both `ShapedLine` and `WrappedLine` have public `paint(origin, line_height, align, align_width | bounds, window, cx)` and `paint_background(..)` (`crates/gpui/src/text_system/line.rs:107-184, 463-525`).
  - `LineLayout::paint(.., &[DecorationRun], ..)` (`crates/gpui/src/text_system/line.rs:390-412`) paints a bare layout. A custom element can shape and paint its own text with these.
- **Splitting shaped lines**: `ShapedLine::split_at` and `cursor().take_until()` (`:195-382`).

**Elements**
- `StyledText`:
  - `StyledText::new(text)`.
  - `.with_runs(Vec<TextRun>)`: the runs must cover the text exactly, otherwise it panics (`text.rs:502-515`).
  - `.with_highlights(impl IntoIterator<Item=(Range<usize>, HighlightStyle)>)`: ranges sorted and non-overlapping; resolved against the inherited style at layout (`:409-427, 537-551`).
  - `.with_default_highlights(&TextStyle, ..)` (`:394-405`).
  - `.with_font_family_overrides(ranges → family)` (`:456-499`).
  - `.layout() -> &TextLayout` (`:387-390`).
- `HighlightStyle { color, font_weight, font_style, background_color, underline, strikethrough, fade_out }` (`crates/gpui/src/style.rs:580`). It has no family, size or features.
- `InteractiveText` (`text.rs:1082-1158`, handlers `:1241-1292`):
  - `InteractiveText::new(id, StyledText)`.
  - `.on_click(Vec<Range<usize>>, Fn(range_ix, &mut Window, &mut App))`: fires when mouse-down and mouse-up land in the same range.
  - `.on_hover(Fn(Option<usize>, MouseMoveEvent, ..))`.
  - `.tooltip(Fn(usize, ..) -> Option<AnyView>)`.
  - Shows a pointing-hand cursor over clickable ranges.
- **No text selection anywhere**: searched "select" in `crates/gpui/src/elements/text.rs`, not found.

**`TextLayout` queries** (`Rc<RefCell<Option<TextLayoutInner>>>`, `text.rs:580-599`)
- It is filled by the measure step. The query methods panic (`.expect`) if called before measure and prepaint.
- `index_for_position(Point) -> Result<usize, usize>` (`:931-962`): `Ok` means the point is on a glyph; `Err` gives the nearest boundary when it is outside.
- `position_for_index(usize) -> Option<Point>` (`:965-993`): the row's top-left plus x, in window coordinates. See the wrap-boundary caveat in 5.6.
- `line_layout_for_index`, `line_layouts() -> SmallVec<[Arc<WrappedLineLayout>;1]>`, `bounds()`, `line_height()`, `len()`, `text()`, `wrapped_text()` (`:996-1078`).
- `paint_background(window, cx)` and `paint_foreground(window, cx)` (`:873-883`): split paint passes, documented as letting callers "paint content, such as highlights, between" them.

**Lower-level hit-testing**
- On `LineLayout` (`crates/gpui/src/text_system/line_layout.rs:62-130`):
  - `index_for_x(x) -> Option<usize>`.
  - `closest_index_for_x(x) -> usize`: the nearest boundary; use it for carets.
  - `x_for_index(ix) -> Pixels` and `font_id_for_index`.
- On `WrappedLineLayout` (`:367-474`):
  - `index_for_position(pos, line_height) -> Result<usize, usize>`.
  - `closest_index_for_position(..)`.
  - `position_for_index(ix, line_height) -> Option<Point>`.
- All of these are linear scans that assume glyph x increases with byte index, so they are approximate for RTL text.

**Selection across many blocks (browser-like)** has to be built by the app:
- Keep a selection model of (block id, byte index) anchors, resolved with `closest_index_for_position`.
- Paint it as quads, using per-row extents from `wrap_boundaries` and `unwrapped_layout.x_for_index`.
- **Don't** implement it as `background_color` highlights. In `shape_text`, a background change splits FontRuns (`text_system.rs:755-785`). That changes the cache key, so every selection change re-shapes. On Windows it also breaks ligatures and kerning at the boundary, because adjacent runs get alternating `next_up()` font sizes (`crates/gpui_windows/src/direct_write.rs:547-572`).

**Truncation and line clamp**
- **API**:
  - `TextOverflow::{Truncate, TruncateStart, TruncateMiddle}(SharedString)` (`crates/gpui/src/style.rs:404-419`).
  - Styled helpers (`crates/gpui/src/styled.rs:89-148`): `text_ellipsis()`, `text_ellipsis_start()`, `text_ellipsis_middle()`, `truncate()` (= overflow_hidden + nowrap + ellipsis) and `line_clamp(n)` (adds overflow_hidden).
- **Accuracy** (`TextMeasure::measure`, `text.rs:683-751`):
  - Single line: truncation is skipped when the shaped text fits. Otherwise the cut point comes from summed per-character advances, with no kerning (`crates/gpui/src/text_system/line_wrapper.rs:240-287, 347-398, 561-584`).
  - `line_clamp` with an ellipsis uses `truncate_wrapped_line`, a character-width reimplementation of wrapping (`crates/gpui/src/text_system/line_wrapper.rs:409-541`). It can disagree with the shaped wrap near the edge.
- **End truncation** trims trailing whitespace and ASCII punctuation before adding the affix (`:385-389`).
- **`line_clamp` without overflow**: it only caps the number of wrap boundaries (`crates/gpui/src/text_system/line_layout.rs:273-278`). The last row keeps all the overflow, which overflow_hidden clips.

**White space**
- `WhiteSpace { Normal, Nowrap }` (`crates/gpui/src/style.rs:394-402`). The element width is used as the wrap width only for `Normal` (`text.rs:674-681`).
- Spaces are never collapsed, and `'\n'` always forces a line break (`text_system.rs:819-848`).
- On Linux, tabs are replaced by spaces for shaping (`[cosmic-text-0.19.0]/src/shape.rs:141-146`).

**Line breaking**
- Shaped text is wrapped by `LineLayout::compute_wrap_boundaries` (`crates/gpui/src/text_system/line_layout.rs:194-292`). Break opportunities come from `icu_segmenter::LineSegmenter::new_for_non_complex_scripts(Default::default())` (`crates/gpui/src/text_system/line_wrapper.rs:543-547`): UAX #14 with baked data, built by a `const fn` (`[icu_segmenter-2.3.0]/src/line.rs:449-464`).
- There is **no** dictionary breaking for Thai, Lao, Khmer or Myanmar.
- Breaks inside shaped clusters are ignored (`crates/gpui/src/text_system/line_layout.rs:210-224`).
- An overlong word breaks at a glyph boundary, roughly CSS `overflow-wrap: break-word` (`:280-286`).
- No hyphenation.

**Bidi**
- **Linux**: cosmic runs `unicode_bidi::BidiInfo` on each line (`[cosmic-text-0.19.0]/src/shape.rs:1348`). GPUI shapes each bidi paragraph separately so they stay on one baseline (`crates/gpui_wgpu/src/cosmic_text_system.rs:597-690`).
- **Windows**: `IDWriteTextLayout` does the reordering, with the default LTR reading direction; `SetReadingDirection` is never called.
- **Gaps**: GPUI's wrapping code notes "the lack of complete UAX #9 support" (`crates/gpui/src/text_system/line_layout.rs:252-254`). There is no paragraph-direction or start/end alignment API.

### 5.8 Measuring text height without painting

- **API** on `WindowTextSystem`, reached with `window.text_system() -> &Rc<WindowTextSystem>` (`window.rs:2829-2831`). There is no paint-phase assertion, so it can be called from render, event handlers or a custom element's measure.
  ```rust
  pub fn shape_text(&self, text: SharedString, font_size: Pixels, runs: &[TextRun],
                    wrap_width: Option<Pixels>, line_clamp: Option<usize>)
      -> Result<SmallVec<[WrappedLine; 1]>>;                       // text_system.rs:727-853
  pub fn shape_line(&self, text: SharedString, font_size: Pixels, runs: &[TextRun],
                    force_width: Option<Pixels>) -> ShapedLine;      // :683-722, no '\n'
  pub fn layout_line<T>(&self, text: T, font_size: Pixels, runs: &[TextRun],
                        force_width: Option<Pixels>) -> Arc<LineLayout>; // :863-880
  ```
- **Matching the element exactly**: this reproduces the `Text`/`StyledText` element's own computation (`text.rs:833-839, 755-773`):
  ```rust
  let font_size = style.font_size.to_pixels(window.rem_size());
  let line_height = window.pixel_snap(style.line_height.to_pixels(font_size.into(), window.rem_size()));
  let lines = window.text_system().shape_text(text, font_size, &runs, Some(content_width), None)?;
  let height: Pixels = lines.iter().map(|l| l.size(line_height).height).sum(); // line_height × Σ(wrap_boundaries + 1)
  ```
  (`WrappedLineLayout::size`, `crates/gpui/src/text_system/line_layout.rs:332-337`; `Window::pixel_snap`, `window.rs:3593-3595`.) Use the same runs and the same content-box width the element will receive.
- **Caching** (`LineLayoutCache`, `crates/gpui/src/text_system/line_layout.rs:490-772`):
  - **Structure**: two maps, `previous_frame` and `current_frame`, keyed by `Arc<CacheKey { text, font_size, runs, wrap_width, max_lines, force_width }>` and holding `Arc<LineLayout>` / `Arc<WrappedLineLayout>`.
  - **Hits**: a hit costs a hash of the text and runs. A hit in the previous frame moves the entry forward.
  - **Frame end**: `finish_frame` (`:596-612`, called each frame at `window.rs:3942`) swaps the maps and drops everything not used this frame, **unless the Arc is still held elsewhere** (`keep_held_layouts`, `:479-488`).
  - **Re-wrapping**: a wrapped-line miss first looks up the unwrapped `LineLayout`, whose key has no wrap width, and only recomputes the wrap boundaries (`:657-669`). Re-wrapping at a new width is cheap while the unwrapped layout is cached or held.
- **Virtual list recipe**: keep each message's `WrappedLine`s (or the `Arc<LineLayout>`) in the model.
  - They then stay cached across frames, and a width change re-wraps without re-shaping.
  - Rough cost (estimated from the struct, not measured): about 32 bytes per `ShapedGlyph`, plus the runs.
- **Element-level reuse**: unchanged text elements skip measuring entirely through `TextMeasure::reuse_measurement`. It compares the text, runs, style, size, scale factor and font generation (`text.rs:614-646`).
- **Measuring outside a window**:
  - `WindowTextSystem::new(cx.text_system().clone())` is public (`text_system.rs:642-653`).
  - Its `finish_frame` is `pub(crate)` (`:855-857`), so a private instance's cache grows without bound. Create one per batch and drop it.
  - It holds `Cell`/`RefCell`, so it is not `Sync`; whether it is `Send` was not compile-checked.
  - Platform shaping takes a global write lock (`crates/gpui_wgpu/src/cosmic_text_system.rs:273-275`; `crates/gpui_windows/src/direct_write.rs:177-186`), so shaping on a background thread serialises with the UI thread.
- **Cost of a cache miss**:
  - Linux: cosmic `ShapeLine::new` (bidi, word split, harfrust per word, font matching), then `layout_to_buffer`, then copying the glyphs (`crates/gpui_wgpu/src/cosmic_text_system.rs:692-851`).
  - Windows, per '\n'-line: UTF-16 conversion, `CreateTextFormat`, `CreateTextLayout`, `GetLineMetrics`, and `Draw` through the COM renderer (`crates/gpui_windows/src/direct_write.rs:479-601`). Colour fonts add one `TranslateColorGlyphRun` per glyph (`:1436-1437`).
  - Wrapping: one ICU segmentation pass plus one pass over the glyphs (`crates/gpui/src/text_system/line_layout.rs:194-292`).
- **Not exact**: `cx.text_system().line_wrapper(font, size)` → `wrap_line(&[LineFragment], width, IndentAdjustment)` sums cached per-character advances, for a single font, with no shaping or kerning (`text_system.rs:516-530`; `crates/gpui/src/text_system/line_wrapper.rs:77-235`). Use it only as a quick estimate.

### 5.9 Fidelity vs Chromium

- **Kerning and ligatures**: on by default (harfrust's defaults; on Windows DirectWrite's, plus liga/clig/calt forced on).
  - Linux shapes word by word (cosmic `ShapeWord`); Blink's word cache mostly does the same.
  - Windows breaks shaping at every FontRun boundary (`crates/gpui_windows/src/direct_write.rs:547-572`).
- **Baseline**: `baseline = (line_height − ascent − descent)/2 + ascent` (`crates/gpui/src/text_system/line.rs:547-548`), the same half-leading model as CSS. Two differences:
  - The ascent and descent are the **maximum over every font used in the whole '\n'-line**, and every wrapped row reuses them (Linux takes a per-glyph maximum, `[cosmic-text-0.19.0]/src/shape.rs:2981-2982`; Windows takes DirectWrite line metrics, `crates/gpui_windows/src/direct_write.rs:538-546`). One emoji or CJK fallback glyph shifts the baseline of the whole paragraph; CSS keeps the primary font's metrics.
  - On Windows, `descent = height − baseline` from `DWRITE_LINE_METRICS` (`:542-545`), and that height includes the font's line gap *(MS docs)*. Fonts with a non-zero lineGap sit differently than on Linux.
- **Line height and sizes**: one `line_height` per element, with no per-row growth. No mixed font sizes within a paragraph: `shape_text` takes one `font_size` and `TextRun` has none. Inline code at 0.85em, or larger inline emoji, need the 5.6 workarounds or separate elements.
- **Mixed fonts inline**: supported per `TextRun.font` (family, weight, style, features).
- **Trailing spaces don't hang**: in `compute_wrap_boundaries` a space glyph's advance counts toward the overflow test (`crates/gpui/src/text_system/line_layout.rs:247, 269-288`). A word whose following space crosses the edge moves to the next row.
  - Traced by hand, not run: "aaa bbb ccc" at width = width("aaa bbb") wraps as "aaa / bbb ccc"; Chromium gives "aaa bbb / ccc".
  - Fix: a small patch that skips the overflow check for whitespace glyphs.
- **Antialiasing and hinting**: comparable defaults. Linux uses vertical-only hinting with ¼-pixel x positioning; Windows uses DirectWrite's natural modes. But GPUI's gamma and contrast are its own (5.5), and Linux ignores per-font fontconfig settings.
- **Missing CSS features**: decoration placement is heuristic (5.5). There is no letter-spacing, justify, hyphenation, vertical-align/superscript or text-shadow.
- **Colour emoji on Linux** depend on Noto Color Emoji's PostScript name (5.6). COLRv1 Noto builds don't render in colour.

### 5.10 Licensing (text path)

No GPL crate is on the text path.
- **GPUI and Zed-internal crates**: every GPUI crate is Apache-2.0 (`crates/gpui*/Cargo.toml:6`). So are the Zed-internal crates they pull in: `collections`, `sum_tree`, `refineable`, `gpui_util`, `scheduler`, `http_client`, `util_macros`.
- **Third-party crates**:

  | Crate | License |
  |---|---|
  | cosmic-text | MIT OR Apache-2.0 |
  | swash | Apache-2.0 OR MIT |
  | fontdb | MIT |
  | harfrust | MIT |
  | skrifa, read-fonts | MIT OR Apache-2.0 |
  | fontconfig-parser | MIT |
  | unicode-bidi | MIT OR Apache-2.0 |
  | unicode-linebreak | Apache-2.0 |
  | icu_segmenter | Unicode-3.0 |
  | zed-font-kit | MIT OR Apache-2.0 |
  | freetype-sys, yeslogic-fontconfig-sys (bindings) | MIT |

- **FreeType itself** is dual-licensed FTL / GPLv2. If it ends up linked, take FTL, which requires a credit line.

### Gaps and workarounds

- **Variable axes**: there is no wght, wdth or opsz control, and on Linux a variable font renders at its default instance. → Ship static instances, one file per weight, with width and opsz cuts under their own family names. Or patch `gpui_wgpu` (5.3).
- **Linux full system-font scan at startup**, plus whole-database work on fallback and font-match misses. → Patch `crates/gpui_linux/src/linux/platform.rs:132` to `CosmicTextSystem::new_without_system_fonts("<our UI family>")`, embed our fonts, and add system fonts lazily from `cx.on_missing_glyphs` (mmap + leak + `add_fonts`). Without the patch, at least call `prewarm_fonts` on a background executor.
- **Family-name collisions**: on Linux a system font with the same family name wins over the embedded one. → Give embedded fonts unique family names, or drop system fonts.
- **Panic when nothing resolves**: `resolve_font` panics if neither the requested family nor any of the fallback stack resolves. → Set the root `TextStyle.font_family` explicitly, and set `system_font_fallback` if system fonts are removed.
- **Twemoji colour on Linux**: needs the `check_is_known_emoji_font` patch (or a PostScript-name rename), plus an `'m'` glyph in the font (or a patch of the `'m'` rule). Windows renders COLR v0 only; CBDT, sbix and SVG fonts come out as black silhouettes.
- **Inline images**: not supported. → Use a spacer-font U+FFFC run and an overlay painted at glyph positions computed from `wrap_boundaries` (`position_for_index` is wrong at row starts).
- **Selection**: there is none. → Build a cross-block selection model in the app and paint quads between `TextLayout::paint_background` and `paint_foreground`. Never express selection as run backgrounds (that re-shapes, and on Windows breaks kerning).
- **Baseline jumps with fallback fonts**: → Ship emoji and fallback fonts whose ascent and descent don't exceed the UI font's, or patch the Linux layout to use the primary font's metrics.
- **Trailing-space wrap differs from CSS**: → Patch `compute_wrap_boundaries` to ignore whitespace glyphs in the overflow test.
- **No letter-spacing**: → A post-pass patch in `LineLayoutCache::layout_line` that shifts each glyph's x and the width (modelled on `apply_force_width_to_layout`, `crates/gpui/src/text_system/line_layout.rs:779-802`). Or avoid letter-spacing in the design.
- **No mixed sizes or per-row line height in a paragraph**: → Use separate elements, or keep inline code and emoji at the paragraph size.
- **Truncation and clamp use character widths, not shaped widths**: they may cut one word differently. → For exact results, measure with `shape_text` and truncate yourself.
- **Windows: `FontFallbacks` can't name embedded fonts, and embedded fonts are copied into RAM**: → Use explicit runs, and keep the embedded font set small.
- **Off-window measuring**: a private `WindowTextSystem` cache never resets. → Create one per batch. Shaping is serialised by the platform lock.
- **Missing-glyph reports are Linux-only**: that's acceptable, since Windows falls back to system fonts on its own.

## 6. Lists and scrolling

Source: Zed `a3ce8d5e88e35697b2ba1cf9cb3c6816089b8281`; paths are relative to the zed root. "Inferred" marks a conclusion drawn from reading code paths, not from a doc comment or test.

### 6.1 What this means for the message list

- **Prepend keeps the reader still.** `ListState`'s scroll position is an item anchor, `ListOffset { item_ix, offset_in_item }` (`crates/gpui/src/elements/list.rs:1448-1453`). Inserting items above it shifts `item_ix` by the inserted count and keeps `offset_in_item` (`crates/gpui/src/elements/list.rs:534-536`), and a test pins this down: `splice(0..0, 2)` moves the anchor from (6, 40) to (8, 40) and the same row paints at the same y (`crates/gpui/src/elements/list.rs:2728-2738`).
- **Heights are exact for every row that has been laid out.** Each item is a separate Taffy root, laid out at the list's definite width with min-content height (`crates/gpui/src/elements/list.rs:1069-1074`, `1092-1093`). Items that were never laid out count as **0 px**, or as a hint you supply (`crates/gpui/src/elements/list.rs:1679-1683`). So the scrollbar and pixel maths are only exact where rows were measured: the visible rows plus the overdraw band.
- **We can drive the wheel ourselves.** Do it with a capture-phase `ScrollEvent` listener plus `cx.stop_propagation()` (the list only listens in the bubble phase, `crates/gpui/src/elements/list.rs:1632`). Move the list with `ListState::scroll_by` or `set_offset_from_scrollbar`, repaint with `cx.notify(view_id)`, and run ticks through `window.on_next_frame` (§6.5.5). Nothing in GPUI animates wheel input, so nothing fights the animation, apart from the gotchas listed in §6.5.6.
- **A full custom list element is not required**, but a small wrapper that owns the wheel is (a `canvas` or a ~60-line `Element`). Write our own virtualised element only if we outgrow the limits in "Gaps and workarounds" (per-frame re-render of visible rows, no row caching, the first visible row is always the anchor).

### 6.2 `list()` + `ListState`

**6.2.1 Types and constructor**

- `pub fn list(state: ListState, render_item: impl FnMut(usize, &mut Window, &mut App) -> AnyElement + 'static) -> List` (`crates/gpui/src/elements/list.rs:24-34`). Zed passes `cx.processor(|this, ix: usize, window, cx| …)`, which `update`s the view per item (`crates/gpui/src/app/context.rs:255-263`; used at `crates/agent_ui/src/threads_archive_view.rs:1091-1092`). The view's render lease has already ended when `prepaint` calls the closure (`crates/gpui/src/view.rs:242-246`), so reading or updating the view from inside it is fine.
- `List` implements `Styled` (`crates/gpui/src/elements/list.rs:1661-1665`). Padding and border become content insets (`crates/gpui/src/elements/list.rs:1654-1659`). It has `.with_sizing_behavior(ListSizingBehavior)` (`crates/gpui/src/elements/list.rs:46-49`).
- `#[derive(Clone)] pub struct ListState(Rc<RefCell<StateInner>>)` (`crates/gpui/src/elements/list.rs:53-54`). Store it on the view and clone it into `list()` each frame.
- `ListState::new(item_count: usize, alignment: ListAlignment, overdraw: Pixels) -> Self` (`crates/gpui/src/elements/list.rs:313-330`). It splices in `item_count` unmeasured items. There is no setter for overdraw. Zed's agent chat uses `ListState::new(0, ListAlignment::Top, px(2048.0))` + `set_follow_mode(FollowMode::Tail)` (`crates/agent_ui/src/conversation_view.rs:1718-1719`).
- `ListAlignment::{Top, Bottom}` (`crates/gpui/src/elements/list.rs:149-154`). `FollowMode::{Normal, Tail}` (`crates/gpui/src/elements/list.rs:98-104`). `ListSizingBehavior::{Infer, Auto (default)}` (`crates/gpui/src/elements/list.rs:173-179`). `ListHorizontalSizingBehavior::{FitList, Unconstrained}` (`crates/gpui/src/elements/list.rs:203-209`) is used only by `uniform_list` (`crates/gpui/src/elements/uniform_list.rs:632-646`), not by `List`.

**6.2.2 Data model**

- `items: SumTree<ListItem>` (`crates/gpui/src/elements/list.rs:65`). `ListItem::Unmeasured { size_hint: Option<Size>, focus_handle }` or `Measured { size, focus_handle }` (`crates/gpui/src/elements/list.rs:230-239`).
- The summary holds `count, rendered_count, unrendered_count, height, has_focus_handles, has_unknown_height` (`crates/gpui/src/elements/list.rs:291-298`). An unmeasured item adds `size_hint.height`, or 0 with `has_unknown_height = true` (`crates/gpui/src/elements/list.rs:1671-1686`).
- Seek dimensions are `Count` (item index) and `Height` (pixels from the top) (`crates/gpui/src/elements/list.rs:1716-1746`). Index-to-pixel and pixel-to-index conversions are O(log n) cursor seeks (e.g. `scroll_top` `crates/gpui/src/elements/list.rs:941-948`).

**6.2.3 Measured vs estimated: what is rendered each frame** (`StateInner::layout_items`, `crates/gpui/src/elements/list.rs:1036-1268`)

- **Lazy rendering.** `render_item` is called only for the items described below. Everything else is just a `SumTree` entry.
- **Visible items:** rendered and laid out again **every frame**, even if already measured. The cached-size shortcut applies only when `visible_height >= available_height`, i.e. in the trailing overdraw (`crates/gpui/src/elements/list.rs:1087-1093`). Only visible items are kept for painting (`crates/gpui/src/elements/list.rs:1107-1116`).
- **Trailing overdraw** (`available_height .. available_height + overdraw`): rendered and laid out only if not yet measured; the element is then dropped unpainted (`crates/gpui/src/elements/list.rs:1081-1093`, `1107`).
- **Leading overdraw** (`overdraw` px above the anchor): the same, measure-only (`crates/gpui/src/elements/list.rs:1194-1217`).
- Measured sizes are written back into the tree (`crates/gpui/src/elements/list.rs:1219-1225`). A width change, or a change in insets, resets **every** item to `Unmeasured { size_hint: None }` (`crates/gpui/src/elements/list.rs:1562-1584`).
- `measure_all(self) -> Self` (`crates/gpui/src/elements/list.rs:335-338`) makes the next prepaint render and lay out **every** item once (`layout_all_items`, `crates/gpui/src/elements/list.rs:995-1034`, called at `crates/gpui/src/elements/list.rs:1281-1286`). It re-arms on `reset`, `remeasure*` and width changes (`crates/gpui/src/elements/list.rs:358`, `460`, `1583`), but not on `splice`, so spliced items are measured lazily.
- `with_uniform_item_height(self, Pixels)` and `reset_with_uniform_height(count, Pixels)` give unmeasured items a height hint for the scrollbar (`crates/gpui/src/elements/list.rs:346-349`, `370-392`).
- `set_item_measurement_readiness(impl Fn(usize, &App) -> bool)` (`crates/gpui/src/elements/list.rs:649-651`): a row reported "not ready" keeps its size only as a hint (`crates/gpui/src/elements/list.rs:242-254`). While such a row is at or above the anchor, the list keeps the anchor and does not backfill above it (`crates/gpui/src/elements/list.rs:1156-1166`, `1189-1192`; test `crates/gpui/src/elements/list.rs:2241-2284`). This fits rows whose images haven't loaded yet. You must request a frame when a row becomes ready (`crates/gpui/src/elements/list.rs:646-648`).
- A focused off-screen item is still rendered (after the visible ones) so keyboard input keeps working, if focus handles were supplied via `splice_focusable` (`crates/gpui/src/elements/list.rs:1239-1261`).

**6.2.4 Alignment, the anchor and `logical_scroll_top`**

- The state is `scroll_anchor: Option<ScrollAnchor { position: ListOffset, pending, provisional, pending_end }>` (`crates/gpui/src/elements/list.rs:66`, `78-83`).
- `logical_scroll_top()` returns the anchor, or with no anchor: Top gives `(0, 0)`, Bottom gives `(item_count, 0)` (`crates/gpui/src/elements/list.rs:926-939`).
- **Bottom with no anchor means "pinned to the end".** Layout walks back from the end and sets the anchor to `None` again (`crates/gpui/src/elements/list.rs:1131-1185`). A wheel scroll or scrollbar drag that reaches the max also sets `None` for Bottom (`crates/gpui/src/elements/list.rs:889-890`, `1420-1421`). A test asserts that `logical_scroll_top().item_ix == item_count` while pinned (`crates/gpui/src/elements/list.rs:3345-3347`).
- **Short content under Bottom** gets a negative `offset_in_item` (`crates/gpui/src/elements/list.rs:1179-1183`). Prepaint subtracts it, so rows sit at the bottom of the viewport (`crates/gpui/src/elements/list.rs:1303`; inferred). Top clamps the offset to 0, so short content sits at the top (`crates/gpui/src/elements/list.rs:1174-1176`).
- **Append while pinned:** with anchor `None`, `splice(count..count, n)` does not touch the anchor. The next layout walks back from the new end, so the view stays at the bottom (inferred from `crates/gpui/src/elements/list.rs:523-537`, `1062-1185`).
- **Append while scrolled up:** the anchor index is below the splice point and nothing moves.
- **Follow mode (either alignment):** `set_follow_mode(FollowMode::Tail)` snaps to the end (`crates/gpui/src/elements/list.rs:604-625`). While following, each layout re-anchors to the end (`crates/gpui/src/elements/list.rs:1052-1060`). It stops on an upward wheel / negative `scroll_by` / `scroll_to(ix < count)` / scrollbar drag (`crates/gpui/src/elements/list.rs:902-904`, `562-564`, `671-673`, `1418`). It re-engages when a layout finds the view within 1 px of the bottom (`crates/gpui/src/elements/list.rs:1230-1237`). Related: `pause_following_tail()` (`crates/gpui/src/elements/list.rs:636-644`) and `is_following_tail()` (`crates/gpui/src/elements/list.rs:655-660`). Test: items grow and the view stays at the bottom (`crates/gpui/src/elements/list.rs:2933-2999`).

**6.2.5 `splice` / `reset` / `remeasure`: exact effect on the anchor**

- `splice(old_range: Range<usize>, count: usize)` = `splice_focusable(old_range, (0..count).map(|_| None))` (`crates/gpui/src/elements/list.rs:489-491`).
- `splice_focusable(old_range, impl IntoIterator<Item = Option<FocusHandle>>)` (`crates/gpui/src/elements/list.rs:497-538`) replaces the range with `Unmeasured { size_hint: None }` items (`crates/gpui/src/elements/list.rs:509-518`). Then:
  - The anchor is inside `old_range`: it moves to `(old_range.start, 0)` and any pending offset is dropped (`crates/gpui/src/elements/list.rs:528-533`).
  - `old_range.end <= anchor.item_ix` (an insert or delete above the anchor, including a prepend at `0..0`): `item_ix = item_ix - old_range.len() + count`, and **`offset_in_item` is unchanged** (`crates/gpui/src/elements/list.rs:534-536`).
  - A splice at or above a provisional anchor cancels the provisional state (`crates/gpui/src/elements/list.rs:524-527`).
- **Walk-through: prepend 50 older messages while reading item k at offset o.**
  1. `splice(0..0, 50)` turns the anchor into (k+50, o).
  2. The next prepaint renders forward from item k+50 at y = −o (`crates/gpui/src/elements/list.rs:1079`, `1303`), so the same row appears at the same y.
  3. Inserted rows inside the leading overdraw are rendered for measurement only (`crates/gpui/src/elements/list.rs:1194-1217`). The rest stay at 0 height, so `scroll_px_offset_for_scrollbar` jumps while the content does not move.
- `reset(count)` sets `reset = true` (wheel events are **dropped** until the next prepaint, `crates/gpui/src/elements/list.rs:876-880`, `1553`), clears the anchor and re-splices everything (`crates/gpui/src/elements/list.rs:354-365`).
- **A height change of the anchor row or below:** visible rows are re-measured every frame automatically (§6.2.3). The anchor row's top stays fixed and later rows move down. This is the same as Chromium's top-most-visible anchoring (inferred).
- **A height change above the anchor:** it never moves the visible content, because the anchor is index-based. Rows in the leading overdraw are not re-rendered while measured (`crates/gpui/src/elements/list.rs:1200-1202`), so tell the list with `remeasure_items(range)`.
- `remeasure_items(range)` marks the range unmeasured but keeps hints. If the anchor row is in range, its `offset_in_item` is kept as `PendingScroll::Absolute`, clamped to the new height (`crates/gpui/src/elements/list.rs:410-461`, `1095-1105`; test `crates/gpui/src/elements/list.rs:2187-2238`).
- `remeasure()` does the same for all items with a proportional offset, for font or zoom changes (`crates/gpui/src/elements/list.rs:398-401`, `424-438`).

**6.2.6 Scrolling API**

- `scroll_to(ListOffset)`: sets the anchor; `item_ix >= count` is clamped to `(count, 0)` (end) (`crates/gpui/src/elements/list.rs:663-676`).
- `scroll_by(distance: Pixels)`: **positive = towards the end** (`crates/gpui/src/elements/list.rs:554-583`). It works from `logical_scroll_top()` through the SumTree, so it is exact only across measured rows. It does not clamp at the end (layout does), and it does **not** notify.
- `scroll_to_end()` sets the anchor to `(count, 0)` (`crates/gpui/src/elements/list.rs:591-598`).
- `scroll_to_reveal_item(ix)` (`crates/gpui/src/elements/list.rs:679-708`) places the item at the top if it is above, else just fully visible.
- Queries:
  - `bounds_for_item(ix) -> Option<Bounds>` in window coordinates; measured items at or after the anchor only (`crates/gpui/src/elements/list.rs:712-739`).
  - `item_is_above_viewport` / `item_is_below_viewport` (`crates/gpui/src/elements/list.rs:812-843`).
  - `viewport_bounds()` (`crates/gpui/src/elements/list.rs:801-803`), `item_count()` (`crates/gpui/src/elements/list.rs:464`).
  - `is_scrolled_to_end() -> Option<bool>` is **`None` while any item has unknown height** (`crates/gpui/src/elements/list.rs:470-485`).
- Scrollbar helpers:
  - `scrollbar_drag_started()` / `scrollbar_drag_ended()` freeze the content height during a drag (`crates/gpui/src/elements/list.rs:745-755`). `is_scrollbar_dragging()` (`crates/gpui/src/elements/list.rs:763-765`).
  - `set_offset_from_scrollbar(Point)`: `-y` is pixels from the top, clamped; it maps to an anchor and Bottom+max means pinned (`crates/gpui/src/elements/list.rs:768-770`, `1389-1433`).
  - `max_offset_for_scrollbar()` (`crates/gpui/src/elements/list.rs:774-777`) and `scroll_px_offset_for_scrollbar()`, which is **correct while pinned** and returns `-max_scroll_offset` (`crates/gpui/src/elements/list.rs:783-798`).
  - Zed's `ui` crate adapts these into its `ScrollableHandle` scrollbar (`crates/ui/src/components/scrollbar.rs:1031-1045`).
- `set_scroll_handler(impl FnMut(&ListScrollEvent, &mut Window, &mut App))` (`crates/gpui/src/elements/list.rs:541-546`). `ListScrollEvent { visible_range, count, is_scrolled, is_following_tail }` (`crates/gpui/src/elements/list.rs:157-169`).
  - It is called **only from the built-in wheel path** (`crates/gpui/src/elements/list.rs:906-921`), not from `scroll_to`, `scroll_by` or the scrollbar.
  - `visible_range` is computed from the pre-scroll top, despite its doc comment (`crates/gpui/src/elements/list.rs:907` passes the captured `scroll_top`).
  - For "load older messages", poll `logical_scroll_top()` in `render` instead.
- Overdraw is a pixel band measured above and below the viewport (`crates/gpui/src/elements/list.rs:308-312`). Rows in it are measured, never painted.

**6.2.7 Built-in wheel handling** (`List::paint`, `crates/gpui/src/elements/list.rs:1609-1651`)

- It is registered in **bubble** phase, *before* painting the children, so children's scroll handlers run first and can `cx.stop_propagation()` (`crates/gpui/src/elements/list.rs:1621-1625`, `1631-1632`; test `crates/gpui/src/elements/list.rs:1904-1955`).
- It requires `hitbox_id.should_handle_scroll(window)`, where the hitbox is inserted in prepaint (`crates/gpui/src/elements/list.rs:1558`).
- Deltas arriving within one frame are coalesced (summed while the sign stays the same, `crates/gpui/src/interactive.rs:646-682`). They are converted with **`pixel_delta(px(20.))`, i.e. a fixed 20 px per line** (`crates/gpui/src/elements/list.rs:1630-1634`), then `StateInner::scroll`:
  - new top = old top − delta.y, clamped to `[0, max]` (`crates/gpui/src/elements/list.rs:882-887`);
  - converted back to an anchor (`crates/gpui/src/elements/list.rs:889-900`);
  - then `cx.notify(view)` (`crates/gpui/src/elements/list.rs:923`). The position jumps instantly; there is no smoothing.
- **Stale-capture race:** the listener uses the `scroll_top` *ListOffset captured at the last prepaint* (`crates/gpui/src/elements/list.rs:1628`, `1635-1636`) and resolves it against the *current* tree (`crates/gpui/src/elements/list.rs:885`).
  - If `splice` inserts rows above between that paint and a wheel event, the captured index now points at a different row and the view jumps by the inserted rows' heights (inferred).
  - This is exactly the "user scrolls up, history page arrives" case. Our own handler, built on `scroll_by` or the scrollbar pair, reads the live anchor and avoids it.

**6.2.8 `ListSizingBehavior`**

- `Auto` (default) is a plain Taffy node from the style (`crates/gpui/src/elements/list.rs:1532-1538`).
- `Infer` sizes the list to `min(total measured height, available)` (`crates/gpui/src/elements/list.rs:1474-1531`). It runs `layout_items` an extra time in `request_layout` at **`AvailableSpace::MaxContent` width** (`available_width: None`, `crates/gpui/src/elements/list.rs:1495-1502`, `1069-1072`), so wrapping rows record unwrapped heights (inferred). Do not use it for chat rows.

### 6.3 `uniform_list` + `UniformListScrollHandle`

- Signature: `uniform_list(id: impl Into<ElementId>, item_count: usize, f: impl Fn(Range<usize>, &mut Window, &mut App) -> Vec<R>) -> UniformList` (`uniform_list.rs:22-55`).
- It measures **one** item (index 0, or `with_width_from_item`) every frame and assumes every row has that height (`uniform_list.rs:1-5`, `283`, `360`, `654-676`). Rows are laid out at `Definite(item_height)` (`uniform_list.rs:508-513`), and only the visible range is rendered (`uniform_list.rs:474-494`).
- Scrolling is the div `Interactivity` scroll: a top-origin pixel offset, with no item anchor (`uniform_list.rs:371-406`).
- `UniformListScrollHandle::new()`, `.scroll_to_item(ix, ScrollStrategy::{Top, Center, Bottom, Nearest})` and the strict/offset variants, `.scroll_to_bottom()`, `.is_scrolled_to_end()` (`uniform_list.rs:134-254`). `.y_flipped(bool)` puts item 0 at the bottom (`uniform_list.rs:686-707`).
- **Use it for fixed-height rows only:** member list, channel list, search hits of fixed height. Not for messages.

### 6.4 `div().overflow_y_scroll()` + `ScrollHandle`

- It needs a stateful div: `div().id(..)` (`crates/gpui/src/elements/div.rs:803`), then `.overflow_y_scroll()` (`crates/gpui/src/elements/div.rs:1582`) and optionally `.track_scroll(&ScrollHandle)` (`crates/gpui/src/elements/div.rs:1596`). Without a handle, the offset lives in element state (`crates/gpui/src/elements/div.rs:2362-2382`).
- **All children are laid out and prepainted every frame.** Each child calls `request_layout` as a Taffy child (`crates/gpui/src/elements/div.rs:2051-2056`), every child is prepainted under `with_element_offset(scroll_offset)` (`crates/gpui/src/elements/div.rs:2131-2143`) and painted, clipped by the overflow mask (`crates/gpui/src/elements/div.rs:2186-2188`, `2649-2651`). There is no virtualisation.
- The offset is a pixel `Point` from the top-left that grows **more negative** going down (`crates/gpui/src/elements/div.rs:4495-4501`). It is clamped to `[-max, 0]` in prepaint (`crates/gpui/src/elements/div.rs:2528-2599`). Prepending children therefore moves the visible content: there is no anchoring.
- `ScrollHandle` (`crates/gpui/src/elements/div.rs:4347-4537`):
  - `offset()`, `set_offset(Point)`, `max_offset()`, `bounds()`;
  - `bounds_for_item(ix)`, `top_item()`, `bottom_item()`, `logical_scroll_top() -> (usize, Pixels)`, `children_count()`;
  - `scroll_to_item(ix)` (minimal), `scroll_to_top_of_item(ix)`, `scroll_to_bottom()`.
  - Items are **direct children** only; for a nested target, use `ScrollAnchor::for_handle(..)` + `.anchor_scroll(..)` (`crates/gpui/src/elements/div.rs:4290-4316`, `1602`).
- Wheel handling: bubble phase, `delta.pixel_delta(window.line_height())` (the text style's line height, `crates/gpui/src/elements/div.rs:3445-3452`; `crates/gpui/src/window.rs:3587-3589`). It **does not stop propagation**, so an outer scroll container also receives the event (`crates/gpui/src/elements/div.rs:3448-3494`, inferred).
- **Use it for** short, non-virtualised panes such as settings pages and profile cards.

### 6.5 Wheel and touchpad input

**6.5.1 Types** (`crates/gpui/src/interactive.rs`)

- `ScrollEvent { position, delta: ScrollDelta, modifiers, touch_phase: TouchPhase, source: ScrollSource }` (`crates/gpui/src/interactive.rs:522-538`).
- `ScrollWheelEvent` is a deprecated alias (`crates/gpui/src/interactive.rs:540-542`). `.on_scroll_wheel` is a deprecated alias of `.on_scroll` (`crates/gpui/src/elements/div.rs:401-406`, `1084-1091`).
- `ScrollDelta::{Pixels(Point<Pixels>), Lines(Point<f32>)}` (`crates/gpui/src/interactive.rs:575-580`). `precise()` is true for `Pixels` (`crates/gpui/src/interactive.rs:627-632`). `pixel_delta(line_height)` (`crates/gpui/src/interactive.rs:635-640`). `coalesce` (`crates/gpui/src/interactive.rs:646-682`).
- Sign convention: **+y = content moves down (scroll towards the top)**. The list does `top − delta.y` (`crates/gpui/src/elements/list.rs:885`).
- `TouchPhase::{Started, Moved (default), Ended, Cancelled}` (`crates/gpui/src/interactive.rs:88-100`).
- `ScrollSource::{Pointer, Touch}`. `Touch` covers touchscreen pans, **including momentum GPUI synthesises** (`crates/gpui/src/interactive.rs:546-555`; `crates/gpui/src/gestures.rs:520-521`, `598-602`). No momentum is synthesised for wheel or touchpad.

**6.5.2 Per platform**

| Platform | Mouse wheel | Touchpad | Phases |
|---|---|---|---|
| Wayland (`wl_seat` v5..9, `crates/gpui_linux/src/linux/wayland/client.rs:940-954`) | `axis_value120`: `Lines(value120/120 × 3)` (`client.rs:2668-2695`), or legacy `axis_discrete`: `Lines(discrete × 3)` (`client.rs:2641-2667`). A plain `axis` with source `wheel` is ignored (`client.rs:2613-2615`). | `axis` with a non-wheel source: `Pixels(value × 3 × −1)`. Note the **3× multiplier** (`client.rs:2608-2640`, `2630`). | Always `Moved` (`client.rs:2707`, `2720`). |
| X11 | XI2 smooth-scroll valuators: `Lines(Δvaluator × 3 / increment)` (`crates/gpui_linux/src/linux/x11/client.rs:1388-1398`, `2521-2530`, `2559-2575`). Core buttons 4-7 give ±3 lines, but are skipped when `POINTER_EMULATED` (`client.rs:1288-1307`; `crates/gpui_linux/src/linux/x11/event.rs:20-32`). | Same valuators, so also `Lines`: **a touchpad cannot be told apart from a wheel on X11**. | Always the default `Moved` (`client.rs:2595`). |
| Windows | `WM_MOUSEWHEEL`: `Lines(Δ/120 × SPI_GETWHEELSCROLLLINES)`; Shift uses `WHEELSCROLLCHARS` on x (`crates/gpui_windows/src/events.rs:148-149`, `559-606`, `608-645`; `crates/gpui_windows/src/system_settings.rs:40`). | Precision touchpad through **DirectManipulation**: `Pixels`, with OS inertia enabled (`crates/gpui_windows/src/direct_manipulation.rs:41-47`, `315-334`). Contacts are claimed only for `PT_TOUCHPAD` (`crates/gpui_windows/src/direct_manipulation.rs:97-106`). Events are drained once per `WM_PAINT` (`events.rs:1391-1401`). | `Started` / `Moved`, then `Ended` with a zero delta (`crates/gpui_windows/src/direct_manipulation.rs:166-194`). |

- Wayland: no handler for `wl_pointer.axis_stop` or `axis_relative_direction` (searched `crates/gpui_linux/src` for `AxisStop` / `axis_stop`: not found). Shift remaps vertical to horizontal (`client.rs:2616-2620`). `SCROLL_LINES = 3.0` (`crates/gpui_linux/src/linux/platform.rs:49`).
- **Line-to-pixel conversion:**
  - `list`: 20 px per line (`crates/gpui/src/elements/list.rs:1634`), so one notch = 60 px on Linux, or with the Windows default of 3 lines.
  - div / uniform_list: `window.line_height()` px per line (`crates/gpui/src/elements/div.rs:3445`).
- **No built-in smooth scrolling or wheel momentum** (searched `crates/gpui/src`, `gpui_linux`, `gpui_windows` for momentum / smooth / fling / inertia: only touchscreen fling in `gestures.rs` and the Windows DirectManipulation inertia flag).

**6.5.3 Dispatch** (`window.rs:6567-6624`)

1. On each scroll event, the window first stores the event's position as the mouse position (`window.rs:6273-6277`).
2. It hit-tests the **last rendered frame's** hitboxes (`window.rs:6568`).
3. Capture phase: listeners in registration order, i.e. ancestors first. `stop_propagation` there **skips the whole bubble phase** (`window.rs:6582-6591`).
4. Bubble phase: listeners in reverse order, i.e. innermost first (`window.rs:6592-6598`).

Listeners exist for one frame only: `on_mouse_event` must be called in paint (`window.rs:6053-6071`). `cx.stop_propagation()` is at `crates/gpui/src/app.rs:2928-2930`. A div paints its own listeners *before* its children (`crates/gpui/src/elements/div.rs:2691-2717`), so **a parent's bubble-phase `.on_scroll` runs after the list's** and cannot pre-empt it. Use capture.

**6.5.4 Frame scheduling**

- `window.on_next_frame(FnOnce(&mut Window, &mut App))` (`window.rs:3082-3088`, callable from event handlers). `Context::on_next_frame(window, FnOnce(&mut V, &mut Window, &mut Context<V>))` (`context.rs:283-292`).
- `window.request_animation_frame()` = `on_next_frame(notify(current_view))` (`window.rs:3102-3105`). It needs `current_view()`, which works only during draw (`window.rs:5955-5958`, `297-305`), so call it in `render`/prepaint/paint, not in an event handler.
- In the platform frame callback, pending next-frame callbacks run **before** the dirty check and draw of that same tick (`window.rs:2051-2068`). A scroll applied in a callback is therefore drawn in that frame. No frame timestamp is passed: use `Instant::now()`. GPUI itself uses `cx.background_executor().now()` (`window.rs:3108`).
- **Inactive windows are throttled to about 30 fps** unless input is high-rate (`WindowOptions::inactive_frame_interval`, default 33.3 ms, `crates/gpui/src/platform.rs:2734-2738`, `2903`; `window.rs:1377-1403`). High-rate means 6 or more invalidating inputs within 100 ms (`window.rs:1405-1431`, `6383`). Set it to `None` if smooth-scrolling an unfocused window must stay at full rate.

**6.5.5 Driving the list from our own wheel animation (sketch, not compiled)**

```rust
// render(): list + a full-size overlay that owns the wheel
div().relative().size_full()
  .child(list(self.list_state.clone(), cx.processor(Self::render_row)).size_full())
  .child(canvas(
      |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal), // prepaint
      { let anim = self.wheel_anim.clone(); let state = self.list_state.clone(); let view = cx.entity_id();
        move |_, hitbox, window, _| window.on_mouse_event(move |e: &ScrollEvent, phase, window, cx| {
          if phase != DispatchPhase::Capture || !hitbox.should_handle_scroll(window) { return; }
          cx.stop_propagation();                 // the list's bubble listener never sees it
          if e.delta.precise() { /* Pixels: apply directly via scroll_px(&state, -dy) */ }
          else { anim.borrow_mut().add_notches(e.delta) /* Lines: retarget the animation */ }
          tick(anim.clone(), state.clone(), view, window); // window.on_next_frame(...) chain
        })}).absolute().inset_0())
// tick: dy = anim.advance(Instant::now()); scroll_px(&state, dy); cx.notify(view);
//       if !anim.done() { window.on_next_frame(..tick again..) }
fn scroll_px(state: &ListState, down: Pixels) {              // safe while Bottom-pinned
    let top = -state.scroll_px_offset_for_scrollbar().y;
    state.set_offset_from_scrollbar(point(px(0.), -(top + down)));
}
```

- APIs used:
  - `canvas(prepaint, paint)` (`crates/gpui/src/elements/canvas.rs:10-20`; `Styled` at `crates/gpui/src/elements/canvas.rs:83-87`), `insert_hitbox` (`window.rs:5899-5913`), `Hitbox::should_handle_scroll` (`window.rs:897-899`);
  - `App::notify(EntityId)` (`crates/gpui/src/app.rs:3323`). `scroll_by` / `scroll_to` / `set_offset_from_scrollbar` only mutate state; they do not repaint.
- **Why `scroll_px` and not `scroll_by`:** with Bottom alignment and no anchor (pinned), `scroll_by` starts from the logical `(count, 0)` rather than the real top (`crates/gpui/src/elements/list.rs:559-570`, `934-937`). An upward `scroll_by(−d)` then lands below the real top, so layout walks back and re-pins (`crates/gpui/src/elements/list.rs:1135-1185`). The upward move is lost when d is less than the viewport height and comes out a viewport height short otherwise (inferred).
- `scroll_px_offset_for_scrollbar` special-cases pinned (`crates/gpui/src/elements/list.rs:786-788`). Once an anchor exists, `scroll_by` is equivalent.
- The pair `scroll_px_offset_for_scrollbar` / `set_offset_from_scrollbar` is only consistent with **zero list padding**: the first excludes padding (`crates/gpui/src/elements/list.rs:847-853`), the second includes it (`crates/gpui/src/elements/list.rs:1401`).
- Alternative: Top alignment with `FollowMode::Tail` (Zed's choice) has a real anchor after every layout (`crates/gpui/src/elements/list.rs:1168-1176`), so `scroll_by` is safe there. But short content then sits at the top.

**6.5.6 What can fight a frame-driven animation**

- Follow-tail re-snaps to the end every layout while following (`crates/gpui/src/elements/list.rs:1052-1060`). Pause it on the first upward step, or rely on negative `scroll_by` / `set_offset_from_scrollbar` stopping it (`crates/gpui/src/elements/list.rs:562-564`, `1418`).
- Child `window.request_autoscroll(bounds)` during prepaint overrides the anchor; the list honours it once per frame (`crates/gpui/src/elements/list.rs:1309-1375`, `1586-1602`; `window.rs:4718-4728`).
- `reset()` drops wheel events until the next prepaint (`crates/gpui/src/elements/list.rs:876-880`); a width change zeroes every height (`crates/gpui/src/elements/list.rs:1562-1584`).
- A step larger than `overdraw` crosses unmeasured rows that count as 0 px (or their hint), so it overshoots (inferred from `crates/gpui/src/elements/list.rs:566-580`). Keep `overdraw` comfortably above one frame's step; Zed uses 2048 px.
- Layout snaps element offsets to device pixels (`window.rs:5879-5892`), and quads snap too (`window.rs:5139-5156`, `3709-3719`). Fractional positions therefore render at device-pixel granularity (same as a compositor).

### 6.6 Custom elements

**6.6.1 The trait** (`crates/gpui/src/element.rs:52-143`)

```rust
pub trait Element: 'static + Sized {
    type RequestLayoutState: 'static;
    type PrepaintState: 'static;
    fn id(&self) -> Option<ElementId>;                       // Some(..) => GlobalElementId passed in
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>>;
    fn request_layout(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>,
                      window: &mut Window, cx: &mut App) -> (LayoutId, Self::RequestLayoutState);
    fn prepaint(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>,
                bounds: Bounds<Pixels>, request_layout: &mut Self::RequestLayoutState,
                window: &mut Window, cx: &mut App) -> Self::PrepaintState;
    fn paint(&mut self, id: Option<&GlobalElementId>, inspector_id: Option<&InspectorElementId>,
             bounds: Bounds<Pixels>, request_layout: &mut Self::RequestLayoutState,
             prepaint: &mut Self::PrepaintState, window: &mut Window, cx: &mut App);
    // optional: a11y_role, write_a11y_info, a11y_synthetic_children, into_any
}
```

- `impl<T: Element> IntoElement for T` is a blanket impl (`crates/gpui/src/element.rs:145-151`). `IntoElement` itself is at `crates/gpui/src/element.rs:154-166`. For builder components, use `RenderOnce` + `#[derive(IntoElement)]` (`crates/gpui/src/element.rs:183-193`).
- Phase order is enforced by `Drawable`: request_layout, then prepaint, then paint; each once (`crates/gpui/src/element.rs:330-362`, `416`, `529`, `571`). An element with an `id` is pushed on the id stack around each phase (`crates/gpui/src/element.rs:376-379`, `404-406`, `435-437`).
- `GlobalElementId` is the id path plus a precomputed hash (`crates/gpui/src/element.rs:220-231`). `ElementId` variants are at `window.rs:8241-8262`; `(&'static str, u64)` and `usize` convert (`window.rs:8302-8378`).

**6.6.2 Window APIs a list element uses**

| API | Phase | Ref |
|---|---|---|
| `request_layout(Style, children: impl IntoIterator<Item=LayoutId>, cx) -> LayoutId` | layout/prepaint | `window.rs:5730-5751` |
| `request_measured_layout(Style, Fn(Size<Option<Pixels>>, Size<AvailableSpace>, &mut Window, &mut App) -> Size<Pixels>)` (re-measured every frame) | layout/prepaint | `window.rs:5762-5775` |
| `request_keyed_measured_layout(Style, key: K: PartialEq, measure)` (skips re-measuring while the key is equal) | layout/prepaint | `window.rs:5786-5805` |
| `AnyElement::layout_as_root(Size<AvailableSpace>, ..) -> Size<Pixels>`; recomputes only if the space changed; panics after paint | prepaint | `crates/gpui/src/element.rs:713-720`, `575-625` |
| `AnyElement::prepaint_at(origin, ..)` (absolute offset) / `prepaint_as_root(origin, space, ..)` / `paint(..)` | prepaint / paint | `crates/gpui/src/element.rs:724-744`, `708-710` |
| `with_element_offset(delta, f)` / `with_absolute_element_offset(abs, f)` | prepaint | `window.rs:4637-4665` |
| `insert_hitbox(bounds, HitboxBehavior::{Normal, BlockMouse, BlockMouseExceptScroll}) -> Hitbox` | prepaint | `window.rs:5899-5913`, `903-959` |
| `on_mouse_event::<E: MouseEvent>(FnMut(&E, DispatchPhase, &mut Window, &mut App))` | paint | `window.rs:6058-6071` |
| `with_content_mask(Option<ContentMask { bounds }>, f)`: intersects with the parent mask; **rectangular only, no corner radii** | prepaint/paint | `window.rs:4618-4633`, `2548-2551` |
| `with_element_state::<S, R>(&GlobalElementId, FnOnce(Option<S>, &mut Window) -> (R, S))` / `with_optional_element_state` | prepaint/paint | `window.rs:4836-4936` |
| `with_global_id(ElementId, f)` / `with_id` / `with_element_namespace` | any draw | `window.rs:3539-3562`, `4781-4790` |
| `paint_quad(PaintQuad)` with `fill(bounds, bg)`, `quad(bounds, radii, bg, borders, border_color, style)`, `outline(..)` | paint | `window.rs:5139-5156`, `8413-8507` |
| `transact(f)`: retryable prepaint that truncates hitboxes, dispatch, element states and line layouts on `Err` | prepaint | `window.rs:4690-4711` |
| `defer_draw(AnyElement, absolute_offset, priority, Option<ContentMask>)` or the `deferred(child)` element: paint on top later (popovers) | prepaint | `window.rs:4959-4982`; `crates/gpui/src/elements/deferred.rs:7-25` |
| `paint_layer(f)` | paint | `window.rs:4992-4999` |

**Element-state lifetime:** state survives only if it is accessed on consecutive frames. It is moved from the previous frame on access and dropped otherwise (`window.rs:1209-1214`, `7962-7973`). Per-row state of off-screen rows is lost, so keep it in our models.

**6.6.3 Minimal virtualised list element (signature skeleton)**

```rust
pub struct MessageList { state: MsgListState /* Rc<RefCell<..>> anchor + SumTree */, style: StyleRefinement,
                         render_row: Box<dyn FnMut(usize, &mut Window, &mut App) -> AnyElement> }
pub struct Prepainted { hitbox: Hitbox, rows: Vec<AnyElement> }
impl Styled for MessageList { fn style(&mut self) -> &mut StyleRefinement { &mut self.style } }
impl Element for MessageList {
    type RequestLayoutState = ();
    type PrepaintState = Prepainted;
    fn id(&self) -> Option<ElementId> { None }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> { None }
    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>,
                      window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default(); style.refine(&self.style);
        (window.request_layout(style, None, cx), ())            // size comes from the parent
    }
    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>,
                _: &mut (), window: &mut Window, cx: &mut App) -> Prepainted {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let avail = size(AvailableSpace::Definite(bounds.size.width), AvailableSpace::MinContent);
        let mut rows = Vec::new(); let mut y = bounds.top() - /*anchor offset*/ px(0.);
        // for ix from anchor while y < bounds.bottom():
        //   let mut el = (self.render_row)(ix, window, cx);
        //   let h = el.layout_as_root(avail, window, cx).height;   // exact height; store in SumTree
        //   window.with_content_mask(Some(ContentMask { bounds }), |w| { el.prepaint_at(point(bounds.left(), y), w, cx); });
        //   rows.push(el); y += h;
        Prepainted { hitbox, rows }
    }
    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>,
             _: &mut (), p: &mut Prepainted, window: &mut Window, cx: &mut App) {
        let hb = p.hitbox.clone();                               // register BEFORE children
        window.on_mouse_event(move |e: &ScrollEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hb.should_handle_scroll(window) { /* animate */ cx.stop_propagation(); }
        });
        window.with_content_mask(Some(ContentMask { bounds }), |w| for r in &mut p.rows { r.paint(w, cx) });
    }
}
```

**6.6.4 `list.rs` as the worked reference** (`crates/gpui/src/elements/list.rs:1455-1652`)

- `id()` / `source_location()` return `None` (`crates/gpui/src/elements/list.rs:1459-1465`).
- `request_layout`: `Auto` gives `window.request_layout(style, None, cx)`; `Infer` gives `request_keyed_measured_layout` keyed on `(max_width, total_height)` (`crates/gpui/src/elements/list.rs:1467-1541`).
- `prepaint`:
  1. insert a hitbox (`crates/gpui/src/elements/list.rs:1558`);
  2. invalidate heights on a width change (`crates/gpui/src/elements/list.rs:1562-1584`);
  3. loop `prepaint_items` until no child autoscroll is requested (`crates/gpui/src/elements/list.rs:1586-1602`).
  `prepaint_items` runs inside `window.transact` (`crates/gpui/src/elements/list.rs:1280`): it lays out rows (`layout_items`), then `prepaint_at(item_origin)` under the list's content mask, with `item_origin.y` starting at `top + padding − offset_in_item` and growing by each row's height (`crates/gpui/src/elements/list.rs:1301-1307`, `1377`).
- `paint`: registers the wheel listener first, then paints the rows under the content mask (`crates/gpui/src/elements/list.rs:1609-1651`).
- The state lives in `Rc<RefCell<..>>` owned by the view, not in element state (`crates/gpui/src/elements/list.rs:1-6`, `53-54`).

### 6.7 Per-frame cost and caching

- **Rebuilt each drawn frame:** the element tree is rebuilt from `render` and dropped after the frame (`crates/gpui/src/element.rs:10-14`). `draw_roots` renders the root view, then lays out, prepaints and paints (`window.rs:4106-4183`).
- **Only when dirty:** a frame draws only when the window is dirty, i.e. a `notify` or `refresh` happened (`window.rs:2068`, `2073-2097`).
- **Visible rows re-render every drawn frame**: `render_item` + `layout_as_root` per visible row (§6.2.3). A wheel tick re-renders every visible message row.
- **View caching:** `Entity::cached(StyleRefinement)` / `AnyView::cached(..)` (`crates/gpui/src/view.rs:46-48`, `250-260`).
  - The subtree is reused only if absolute `bounds`, `content_mask`, `text_style` and `opacity` are all equal, the view is not dirty and the window isn't refreshing (`crates/gpui/src/view.rs:324-329`, `544-560`). Reuse replays the recorded prepaint/paint ranges (crate-private `reuse_prepaint` / `reuse_paint`, `window.rs:4450`, `4516`; `crates/gpui/src/view.rs:553`, `639`).
  - A cached view is laid out from the given style, not measured from content, so it **needs a definite size** (`crates/gpui/src/view.rs:250-256`).
  - So: cache the *whole message pane* (sibling updates such as composer typing or the member list then skip it; `notify` dirties only the notified view's ancestors, `window.rs:2569-2580`), **not individual rows**: bounds change on every scroll step and rows need content height.
- **Layout cache:** Taffy nodes persist across frames, keyed by element-id path plus request order, so unchanged subtrees skip recomputation (`crates/gpui/src/taffy.rs:134-181`, `304-368`).
  - Elements without ids are matched by order inside the nearest id'd ancestor (`crates/gpui/src/taffy.rs:149-153`). Give each row root a stable id (`div().id(("msg", msg_id))`) so the cache follows the row as the window slides (inferred).
  - Text uses a keyed measure, so unchanged text isn't re-measured (`crates/gpui/src/elements/text.rs:819-824`; `crates/gpui/src/taffy.rs:282-300`).
  - The line-shaping cache keeps the previous frame's shaped lines (`crates/gpui/src/text_system/line_layout.rs:507-518`, `596-608`).
- **Allocation:**
  - Every `AnyElement` is bump-allocated in the per-App `Arena` (`crates/gpui/src/element.rs:664`, `677`; 1 MiB chunks, `crates/gpui/src/app.rs:1104`; `crates/gpui/src/arena.rs:97-104`, `161-199`). The arena is cleared after each draw and present (`window.rs:2086-2089`, `463-495`), and its chunks are reused.
  - Interior allocations still hit the global heap: the boxed `StyleRefinement` (`uniform_list.rs:48`), boxed listeners (`window.rs:6064`), child vectors (`crates/gpui/src/elements/div.rs:2051-2055`).
- **Layout tree lifecycle:** at the end of a frame, Taffy nodes nobody requested are evicted (`crates/gpui/src/taffy.rs:235-241`). The cache is cleared on a scale-factor change (`crates/gpui/src/taffy.rs:218-227`).

### Gaps and workarounds

- **No smooth wheel and no wheel momentum.** Wheel steps are instantaneous at 20 px per line (`crates/gpui/src/elements/list.rs:1634`); touchpad momentum exists only on Windows (OS inertia). Workaround: the capture-phase interceptor plus `on_next_frame` animation in §6.5.5. Animate only `Lines`; apply `Pixels` directly.
- **Wayland touchpad deltas are multiplied by 3** (`crates/gpui_linux/src/linux/wayland/client.rs:2630`), and there are no phases or `axis_stop`. Rescale `Pixels` by 1/3 on Wayland if Chromium parity matters. Wayland reports no end-of-gesture, so we cannot synthesise momentum there.
- **X11 touchpads arrive as `Lines`** (`crates/gpui_linux/src/linux/x11/client.rs:2577-2598`), so an X11 touchpad would be animated like a wheel. Accept it, or treat small fractional `Lines` as precise (heuristic).
- **Stale-capture race in the built-in wheel handler** after a `splice` (§6.2.7). Workaround: always intercept the wheel ourselves (we must anyway for smoothing).
- **The Bottom-pinned logical top is `(count, 0)`:** `scroll_by`, `scroll_to_reveal_item`, `bounds_for_item` (returns `None`) and `item_is_above_viewport` (reports `true` for every row) all misbehave while pinned (`crates/gpui/src/elements/list.rs:688-690`, `718-720`, `815-819`). Workaround: the scrollbar pixel pair (§6.5.5), or Top alignment with `FollowMode::Tail` as Zed's agent panel does (`conversation_view.rs:1718-1719`), at the cost of short content sitting at the top.
- **Unmeasured rows count 0 px**, so the scrollbar thumb and `is_scrolled_to_end` (which returns `None`) are unreliable after loading history. Workaround: `reset_with_uniform_height` / `with_uniform_item_height` for a decent estimate; avoid `measure_all` on long histories (it renders every row and re-runs after each resize).
- **The anchor is always the first visible row.** Growth inside the viewport (image loads) pushes later rows down. Workaround: report rows with pending media as not ready via `set_item_measurement_readiness`, or reserve media boxes from known dimensions (attachments already carry sizes) so heights never change.
- **`set_scroll_handler` fires only for built-in wheel scrolls.** Once we intercept, it never fires. Workaround: compute "near top, load more" in `render` from `logical_scroll_top()`.
- **Visible rows are re-rendered every frame and cannot be view-cached.** Workaround: keep `render_row` cheap (precomputed display models, stable row ids for Taffy reuse); cache the whole pane view. If profiling shows row rendering dominates during scroll animation, that is the trigger for a custom element that keeps prepainted rows and only re-offsets them. That is not possible with public APIs today: `reuse_prepaint` / `reuse_paint` are crate-private (`window.rs:4450`, `4516`), so it would mean re-prepainting anyway (inferred).
- **`ContentMask` is rectangular only** (`window.rs:2548-2551`), so there is no rounded clipping of the list viewport. Draw rounded frames as overlays.
- **Unfocused windows animate at about 30 fps by default.** Set `WindowOptions { inactive_frame_interval: None, .. }` (`platform.rs:2734-2738`).
- **`request_animation_frame` panics or asserts outside draw** (`window.rs:5955-5958`). From event handlers, use `window.on_next_frame` / `Context::on_next_frame`.

## 7. Text input

Paths are relative to the Zed clone root (`gpui-client/vendor/zed`, commit `a3ce8d5e`, 2026-11-26) unless they start with `gc/`, which means the vendored gpui-kit 0.11.0 crates under `gpui-client/vendor/scratch/gpui-component-check/vendor/` (unpacked from crates.io).

### 7.1 Who does what: `EntityInputHandler` → `ElementInputHandler` → `Window::handle_input`

- GPUI has no text widget. It gives you a protocol: your view implements `EntityInputHandler`, your custom `Element` registers it with `window.handle_input(..)` during **paint**, and each platform backend drives it with insert, IME and geometry calls. All offsets crossing this boundary are **UTF-16 code units** (`crates/gpui/src/platform.rs:2428-2431`). The view stores UTF-8 and converts.
- `pub trait EntityInputHandler: 'static + Sized` (`crates/gpui/src/input.rs:26`). Every method also takes `window: &mut Window, cx: &mut Context<Self>`:
  - `text_for_range(&mut self, range_utf16: Range<usize>, adjusted_range: &mut Option<Range<usize>>) -> Option<String>` (`input.rs:28`)
  - `selected_text_range(&mut self, ignore_disabled_input: bool) -> Option<UTF16Selection>` (`input.rs:37`). `UTF16Selection { range, reversed }` (`platform.rs:2419`).
  - `marked_text_range(&self) -> Option<Range<usize>>` (`input.rs:45`). The IME preedit range.
  - `unmark_text(&mut self)` (`input.rs:52`)
  - `paste(&mut self, item: ClipboardItem)`: default inserts `item.text()` (`input.rs:55-59`). Only platforms that deliver paste as an event (web) call it. On desktop, paste is your own action.
  - `replace_text_in_range(&mut self, range_utf16: Option<Range<usize>>, text: &str)` (`input.rs:62`). Commits text. `None` means "the marked range, else the selection" (example convention, `crates/gpui/examples/input.rs:356-360`).
  - `replace_and_mark_text_in_range(&mut self, range_utf16: Option<Range<usize>>, new_text: &str, new_selected_range: Option<Range<usize>>)` (`input.rs:71`). Sets the preedit. `new_selected_range` is relative to the inserted text (`crates/gpui/examples/input.rs:394-403`).
  - `bounds_for_range(&mut self, range_utf16: Range<usize>, element_bounds: Bounds<Pixels>) -> Option<Bounds<Pixels>>` (`input.rs:81`). Window coordinates. Used to place the IME candidate window (`platform.rs:2505-2514`).
  - `selection_bounds_for_range(..) -> Vec<Bounds<Pixels>>`: default empty (`input.rs:90-98`). Native selection handles, mobile only.
  - `character_index_for_point(&mut self, point: Point<Pixels>) -> Option<usize>` (`input.rs:101`)
  - Defaulted, mostly for mobile and web: `set_selected_text_range` (no-op, `input.rs:109`), `text_length_utf16` (`None`, `input.rs:118`), `accepts_text_input` (`true`, `input.rs:127`), `text_input_configuration` (`input.rs:132`), `text_input_editable_range` (`input.rs:141`).
- `ElementInputHandler<V>` (`input.rs:152-167`). `ElementInputHandler::new(element_bounds, view: Entity<V>)` adapts `EntityInputHandler` to the object-safe `InputHandler` (`platform.rs:2432`) by `view.update(..)` per call (`input.rs:169-319`), passing the paint-time `element_bounds` to `bounds_for_range`. Its `prefers_ime_for_printable_keys` returns `accepts_text_input` (`input.rs:297-300`), so a field routes printable keys to the IME before keybindings.
- `Window::handle_input(&mut self, focus_handle: &FocusHandle, input_handler: impl InputHandler, cx: &App)` (`crates/gpui/src/window.rs:5995-6009`). Only valid during paint (`debug_assert_paint`). It registers the handler **only if `focus_handle.is_focused(window)`** (`window.rs:6003`), wrapped in `PlatformInputHandler::new(..).with_focus(id)` (`window.rs:6005-6007`). After the frame, the window hands the focused handler to the platform window via `PlatformWindow::set_input_handler`/`take_input_handler` (`platform.rs:1322-1323`, `window.rs:3879-3925`).
- After edits your app makes (not ones the platform made), call `window.notify_text_input_changed(&focus)` / `notify_text_selection_changed(&focus)` (`window.rs:6015`, `6026`; contract at `input.rs:16-18`). On Linux and Windows these reach `PlatformWindow::text_input_state_changed`, a default no-op (`platform.rs:1526`). Only the test, iOS and web windows override it (grep `fn text_input_state_changed` → `platform/test/window.rs`, `gpui_ios`, `gpui_web`). Calling them is still correct and keeps the code portable.
- `TextInputConfiguration { autocorrect, autocapitalize, suggestions, input_action }` (`platform.rs:2641-2650`) is re-queried every frame and forwarded on change (`window.rs:6037-6051`). Linux and Windows ignore it: `set_text_input_configuration` is a default no-op (`platform.rs:1328`).
- IME candidate position:
  - `PlatformInputHandler::compute_ime_candidate_bounds` (`platform.rs:2271-2304`) asks `bounds_for_range(caret..caret)` at the start of the marked range on the caret's visual line. Without a composition it uses the selection head.
  - `window.invalidate_character_coordinates()` (`window.rs:7303-7312`) pushes that to `PlatformWindow::update_ime_position` on the next frame. Call it when the caret moves by scrolling or layout.

### 7.2 What keystrokes reach the field

- A key-down first goes through keymap dispatch along the focus path. GPUI then inserts the key's text only if nothing handled it:
  - `dispatch_key_event` → `input_preference` (`window.rs:6628-6645`): `CharacterInput` when the platform flagged `prefer_character_input` and the handler `accepts_text_input`.
  - Bindings are matched with pending multi-stroke handling (`window.rs:6733-6784`).
  - If the event still propagates and `keystroke.key_char` is `Some` with modifiers ⊆ Shift, the platform window calls `input_handler.replace_text_in_range(None, key_char)`: Wayland `crates/gpui_linux/src/linux/wayland/window.rs:1610-1633`, X11 `crates/gpui_linux/src/linux/x11/window.rs:1218-1240`.
  - On Windows, an unhandled `WM_KEYDOWN` returns 1 (`crates/gpui_windows/src/events.rs:448-452`), `TranslateMessage` (`crates/gpui_windows/src/platform.rs:599`) makes `WM_CHAR`, and `handle_char_msg` inserts it (`events.rs:472-479`).
- **Consequence:** editing keys (backspace, arrows, home/end, copy/paste, select-all, Enter) are never delivered as text. The field must define actions, bind them with a key context (`KeyBinding::new("backspace", Backspace, Some("TextInput"))`), and handle them with `.on_action(..)` (`crates/gpui/examples/input.rs:19-37`, `645-662`, `757-771`). A binding on a printable key (e.g. `space` in a parent context) steals that key from the field.
- `Window::dispatch_keystroke(keystroke, cx)` (`window.rs:6178-6201`) synthesizes typing: `keystroke.with_simulated_ime()` fills `key_char` for printable keys, space → `" "`, enter → `"\n"`, tab → `"\t"` (`crates/gpui/src/platform/keystroke.rs:268-288`). If not handled, it calls `PlatformInputHandler::dispatch_input` = `replace_text_in_range(None, ..)` (`platform.rs:2267-2269`). Tests use it.
- `window.show_character_palette()` (`window.rs:3499`) is a no-op on Linux and Windows (default `platform.rs:1442`, only macOS overrides). `request_virtual_keyboard` (`window.rs:3367`) is for mobile.

### 7.3 IME per platform

- **Wayland: `zwp_text_input_v3` (text-input-unstable-v3)** (`crates/gpui_linux/src/linux/wayland/client.rs:57-61`):
  - One `zwp_text_input_v3` per seat (`client.rs:1986-1996`). Enabled on the protocol's `Enter` and disabled on `Leave` (`client.rs:2294-2301`). The session is per surface, not per field.
  - `enable_ime` sends `enable`, `set_content_type(ContentHint::None, ContentPurpose::Normal)` and the cursor rectangle (`client.rs:713-737`). Content type is always `None`/`Normal`, so no password purpose, since `TextInputConfiguration` is not forwarded.
  - `PreeditString` is stored and applied on `Done` as `ImeInput::SetMarkedText` → `replace_and_mark_text_in_range(None, text, None)`. An empty `Done` deletes the marked range (`client.rs:2327-2355`, `crates/gpui_linux/src/linux/wayland/window.rs:1493-1518`). **Preedit cursor and style hints are dropped:** `new_selected_range` is always `None`.
  - `CommitString`: if the text is exactly one byte it is re-injected as a synthetic `KeyDown` so keybindings see it ("IBus intercepts keys… needed for vim mode"). Otherwise it is inserted with `replace_text_in_range` (`client.rs:2302-2326`).
  - Cursor rectangle: `set_cursor_rectangle` from `get_ime_area()` → `ime_candidate_bounds()` after each `Done` (`client.rs:2338-2349`, `crates/gpui_linux/src/linux/wayland/window.rs:1520-1529`) and from `update_ime_position` (`client.rs:755-765`).
  - **No surrounding text:** grep for `surrounding` in `gpui_linux/src/linux/wayland/` finds nothing (`set_surrounding_text`, `delete_surrounding_text` unused). IMEs that rely on context (some predictive or Korean input) get none.
  - Dead keys and compose sequences without an IME go through xkb compose and show as marked text (`client.rs:2173-2211`).
- **X11: XIM** via the `xim` crate, Zed's fork `zed-xim` 0.4.0-zed, pinned by git rev (`crates/gpui_linux/Cargo.toml:127-130`):
  - `X11rbClient::init` at startup (`crates/gpui_linux/src/linux/x11/client.rs:493-495`). An input context with `InputStyle::PREEDIT_CALLBACKS` is created on `FocusIn` (`crates/gpui_linux/src/linux/x11/client.rs:1064-1074`, `821-863`) and reset on `FocusOut` (`crates/gpui_linux/src/linux/x11/client.rs:1075-1090`, `865-875`).
  - Key events are filtered through the IM and forwarded (`crates/gpui_linux/src/linux/x11/client.rs:753-795`, `1530-1559`).
  - Callbacks in `x11/xim_handler.rs`: `handle_commit` (`:69`), `handle_preedit_draw` (`:108`). They become `handle_ime_commit` → `replace_text_in_range` and `handle_ime_preedit` → `replace_and_mark_text_in_range(None, text, None)` (`crates/gpui_linux/src/linux/x11/window.rs:1246-1290`, `crates/gpui_linux/src/linux/x11/client.rs:1561-1604`).
  - Candidate position: `SpotLocation` = bottom-right of `ime_candidate_bounds` (`crates/gpui_linux/src/linux/x11/client.rs:1585-1604`, `277-318`). Without an XIM server it falls back to xkb compose (`crates/gpui_linux/src/linux/x11/client.rs:1185-1195`).
- **Windows: IMM32, not TSF.** It handles `WM_IME_STARTCOMPOSITION` and `WM_IME_COMPOSITION` (`crates/gpui_windows/src/events.rs:153-154`):
  - `GCS_RESULTSTR` → `replace_text_in_range`. `GCS_COMPSTR` → `replace_and_mark_text_in_range` with the caret from `GCS_CURSORPOS` (`events.rs:731-775`).
  - Candidate and composition window: `ImmSetCompositionWindow(CFS_POINT)` + `ImmSetCandidateWindow(CFS_CANDIDATEPOS)` at the caret from `selected_text_range` + `bounds_for_range` (`events.rs:647-694`).
  - IME on or off per frame: after each paint, `update_ime_enabled` associates or dissociates the HIMC based on `accepts_text_input` (`events.rs:696-724`, `1421`).
  - grep for `ITfThreadMgr|TextServices|tsf` in `crates/gpui_windows/src` finds nothing, so there is no TSF.
- **What our field must implement for IME to work everywhere:**
  - A correct UTF-16 ↔ UTF-8 mapping.
  - `marked_text_range` + `replace_and_mark_text_in_range` + `unmark_text`.
  - `bounds_for_range` from the last layout.
  - Drawing the marked run underlined (`crates/gpui/examples/input.rs:527-552`).

### 7.4 Walkthrough: `crates/gpui/examples/input.rs` (912 lines, Apache-2.0)

- **State** (`:39-49`): `content: SharedString`, `selected_range: Range<usize>` (UTF-8), `selection_reversed`, `marked_range`, `last_layout: Option<ShapedLine>`, `last_bounds`, `is_selecting`.
- **Actions** (`:19-37`, bound at `:757-771`): Backspace, Delete, Left, Right, SelectLeft, SelectRight, SelectAll, Home, End, ShowCharacterPalette, Paste, Cut, Copy. The bindings are macOS-flavoured (`cmd-a`, `cmd-v`…). A Linux/Windows app binds `ctrl-*`.
- **Grapheme movement:** `previous_boundary`/`next_boundary` use `unicode_segmentation::grapheme_indices(true)` (`:260-273`). It is O(n) per step, fine for short text.
- **Selection:** anchor/head through `selection_reversed` (`:208-220`). Mouse: down / drag / up with `index_for_mouse_position` → `ShapedLine::closest_index_for_x` (`:113-142`, `:190-206`). Shift-click extends.
- **Clipboard:** `cx.read_from_clipboard()` / `cx.write_to_clipboard(ClipboardItem::new_string(..))`. Paste replaces `\n` with a space (`:153-173`).
- **IME:** the full `EntityInputHandler` impl (`:288-461`). UTF-16 conversion walks chars (`:222-258`). The marked text is drawn as a separate underlined `TextRun` (`:527-552`). `bounds_for_range` uses `ShapedLine::x_for_index` (`:410-432`).
- **Rendering:**
  - A custom `Element` (`TextElement`, `:463-637`) requests one line of height (`:485-496`).
  - It shapes with `window.text_system().shape_line(text, font_size, &runs, None)` (`:555-557`).
  - It builds caret and selection quads in `prepaint`.
  - In `paint` it calls `window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx)`, then paints and stores `last_layout`/`last_bounds` back into the entity (`:596-636`).
  - Key context `"TextInput"` + `track_focus` + `CursorStyle::IBeam` (`:639-674`).
- **Tests** (`:837-912`, need `--features test-support`): UTF-16 selection with emoji/CJK, marked-text replacement semantics, and single-fragment selection geometry for 10k chars via `cx.add_window_view` + `run_until_parked`.
- **Not in the example** (all needed for Decibell):
  - Multi-line and wrapping (single `ShapedLine`, fixed height).
  - Horizontal scrolling: text overflowing the box is clipped, and the caret can leave view.
  - Word movement and deletion (ctrl-left/right, ctrl-backspace).
  - Double-click word / triple-click line selection (`MouseDownEvent::click_count` is available).
  - Undo/redo.
  - Up/down, page up/down.
  - Linux primary selection (`cx.write_to_primary`/`read_from_primary`, `crates/gpui/src/app.rs:2029-2036`).
  - Caret blink.
  - Placeholder styling, password masking, max length, read-only/disabled.
  - Enter/Shift+Enter semantics.
  - Accessibility value.
  - Rope or large-text performance: every edit rebuilds the whole `SharedString`, and UTF-16 conversions are O(n).
  - Calling `invalidate_character_coordinates` on scroll.
- GPUI pieces available for the missing parts:
  - `TextSystem::shape_text(text, font_size, runs, wrap_width: Option<Pixels>, line_clamp: Option<usize>) -> Result<SmallVec<[WrappedLine; 1]>>` (`crates/gpui/src/text_system.rs:727-734`).
  - `WrappedLineLayout::{index_for_position, closest_index_for_position, position_for_index, wrap_boundaries, size}` (`crates/gpui/src/text_system/line_layout.rs:297`, `332-447`).
  - `ShapedLine` / `LineLayout::{x_for_index, closest_index_for_x, index_for_x}` (`crates/gpui/src/text_system/line_layout.rs:62-109`).

### 7.5 Does GPUI ship a reusable field?

- **No.** `crates/gpui/src/elements/` contains only `anchored, animation, canvas, container_query, deferred, div, image_cache, img, list, surface, svg, text, uniform_list`. `text.rs` has `StyledText`/`InteractiveText` for display (`crates/gpui/src/elements/text.rs:367`, `1082`), with no editing.
- grep `impl EntityInputHandler|impl InputHandler for` in `crates/gpui/src` finds only test views (`crates/gpui/src/key_dispatch.rs:739`, `:1209`, `crates/gpui/src/window/native_view.rs:814`) and the adapter in `input.rs`.
- Zed's editors are GPL and off-limits: `crates/editor/Cargo.toml:6` and `crates/ui_input/Cargo.toml:6` are `GPL-3.0-or-later`, as are `ui` and `theme` (their `LICENSE-GPL`).
- Apache-2.0 crates we may use: `gpui`, `gpui_macros`, `gpui_platform`, `gpui_wgpu`, `gpui_linux`, `gpui_windows`, `gpui_util`, `gpui_shared_string`, `sum_tree`, `collections`, `refineable`, `scheduler`, `http_client`, `util` (each has `LICENSE-APACHE` and `license = "Apache-2.0"`).

### 7.6 Third-party: `gpui-component` (longbridge, now "GPUI Kit")

- **Identity:**
  - `gpui-component` 0.11.0, published 2026-11-23, **Apache-2.0** (https://crates.io/api/v1/crates/gpui-component).
  - Repository moved: `longbridge/gpui-component` redirects to https://github.com/longbridge/gpui-kit (~21.4k stars, last push 2026-11-26, not archived).
  - The repo root has `LICENSE-APACHE`. `LICENSE-DOCS.md` adds CC BY 4.0 for website prose only and says code stays Apache-2.0 (https://raw.githubusercontent.com/longbridge/gpui-kit/main/LICENSE-DOCS.md). GitHub reports the licence as "Other" only because of that second file.
  - Cadence: a minor release about weekly (0.7.2 2026-10-12 … 0.11.0 2026-11-23), with frequent breaking changes.
- **Crate split:**
  - `gpui-component` (styled widgets) on top of `gpui-base` 0.11.0 ("behavior, interaction, infrastructure").
  - `gpui-component-macros` and `gpui-kit-assets`.
  - The editing engine lives in **gpui-base**: `gc/gpui-base-0.11.0/src/input/` (~33.6k lines incl. a code editor).
- **Which gpui it uses:** not Zed git and not crates.io `gpui` 0.2.2 (2025-10-22, stale). The workspace pins `gpui = { package = "gpui-pre", version = "=0.3.14" }` (+ `gpui-pre-platform`, `-macros`, `-sum-tree`, … all `=0.3.14`) (https://raw.githubusercontent.com/longbridge/gpui-kit/main/Cargo.toml lines 62-80).
  - `gpui-pre` is a weekly crates.io republish of Zed's Apache crates by a gpui-kit maintainer. 0.3.14 = "snapshot of zed@f126950" (2026-11-22T20:21Z).
  - The GitHub compare API says our pin `a3ce8d5e` is **79 commits ahead, 0 behind** f126950 (`/repos/zed-industries/zed/compare/f126950...a3ce8d5e`).
  - The source diff of `gpui-pre-0.3.14/src` against our `crates/gpui/src` touches 18 files (~4.6k ±lines). One public API change: `request_measured_layout` → `request_keyed_measured_layout`. gpui-component does not use it.
  - An optional `gpui-fast` feature swaps in another gpui fork (`gpui-fast` 0.1.19). Leave it off.
- **Input components:**
  - Single-line: `InputState = InputBaseState<InputMode>` (`gc/gpui-base-0.11.0/src/input/input/mod.rs:10`). Widget `Input::new(&Entity<InputState>)` (`gc/gpui-component-0.11.0/src/input/input.rs:241`).
  - Multi-line: `TextareaState = InputBaseState<TextareaMode>` (`gc/gpui-base-0.11.0/src/input/textarea/mod.rs:10`). Widget `Textarea::new(&Entity<TextareaState>)` (`gc/gpui-component-0.11.0/src/input/textarea.rs:70`).
  - Also `EditorState`/`Editor` (code editor), `NumberInput`, `OtpInput`.
  - Builders:
    - `InputState::new(window, cx)` (`gc/gpui-base-0.11.0/src/input/base/state.rs:13578`)
    - `TextareaState::new(..).rows(n)` / `.auto_grow(min, max)` (`state.rs:13826-13880`)
    - `.placeholder(..)` (`state.rs:1114`)
    - `.submit_on_enter(bool)` (`state.rs:1530`)
    - `.mask_pattern(..)` (`state.rs:4411`), plus masked/password state (`state.rs:518`)
    - `set_value`, `insert`, `replace` (`state.rs:1312-1392`), `value()` (`state.rs:1683`), `focus()` (`state.rs:1766`)
  - Events: `InputEvent::{Change, PressEnter { secondary, shift }, Focus, Blur}` (`state.rs:141-146`). Enter semantics fit a chat composer: with `submit_on_enter`, Enter emits `PressEnter` and Shift+Enter inserts a newline. Ctrl/Cmd+Enter always submits (`state.rs:2487-2491`).
  - Engine:
    - Text in **ropey `=2.0.0-beta.1`** (`gc/gpui-base-0.11.0/src/input/base/rope_ext.rs:3`).
    - Undo/redo with transaction coalescing, max 1000 (`gc/gpui-base-0.11.0/src/input/base/undo_manager.rs:6`).
    - Word movement and deletion, line/document movement, select-to-*, multi-cursor, soft wrap, IME via `impl EntityInputHandler for InputBaseState<M>` (`state.rs:4813`, `bounds_for_range` at `:5314`), caret blink, touch selection, an a11y text value, i18n.
  - Default bindings under key context `"Input"` include both `cmd-*` and `ctrl-*` variants: ctrl-z/y, ctrl-left/right, ctrl-backspace, ctrl-a/c/x/v (`state.rs:167-310`).
- **Dependency weight:**
  - Default features have **no tree-sitter or syntect**. Both are opt-in features, plus `speech` (cpal) (https://crates.io/api/v1/crates/gpui-component/0.11.0/dependencies).
  - Measured with `cargo tree -e normal` on the Linux target: our check crate resolves **471 unique crates vs 397 for plain `gpui` + `gpui_platform`** (the existing `scratch/hello` crate), about **+74**.
  - Notable additions: `ropey`, `lsp-types`, `markdown`, `html5ever`/`markup5ever_rcdom`/`xml5ever`, `notify` (file watcher), `rust-i18n` (+ toml, globwalk, ignore), `jiff`, `similar`, `rust-embed`, `ashpd` settings, `uuid`, `regex`.
  - **Measured binary and idle cost** (same release profile as the hello-world: `opt-level=3`, fat LTO, cu=1, abort, strip; `src/main.rs` of the check crate = `gpui_component::init` + `Root` + one `Input` + one auto-grow `Textarea`; binary kept as `gpui-client/vendor/scratch/bins/gpui-component-input`): **29.66 MB stripped vs 19.29 MB** for the hello-world (**+10.4 MB**, i.e. LTO cannot drop what `gpui_component::init` registers). Idle on Wayland with EGL hidden (§13): Rss 114.0 MB / Pss 72.3 MB / private 66.4 MB (hello-world: 110.1 / 68.0 / 62.1), GPU 62 MiB, 0 frames, main thread 0 wakeups, but **one extra thread `blocking-1` (the `blocking` crate's pool) wakes 2×/s** at idle — source not identified; find and remove it before adopting the crate wholesale. Vendoring only the input engine (`gpui-base`'s `input/`) should avoid most of the +10 MB (not measured).
  - `cargo check` of gpui-base + gpui-component on top of an already-checked gpui took under a minute with 4 jobs.
- **Build against our pin (item A3), done:** `gpui-client/vendor/scratch/gpui-component-check/`. Steps:
  1. `[patch]` cannot do it: the requirement is `gpui-pre =0.3.14` while our gpui is `0.2.2` under another package name, so the patch would be unused. Instead the four gpui-kit crates were unpacked from crates.io into `vendor/`.
  2. `rewrite_manifests.py` repoints `gpui`/`gpui_macros`/`sum-tree` to the clone's `crates/gpui`, `gpui_macros`, `sum_tree` by path and drops the optional `gpui-fast` and dev-deps. The check crate repeats the Zed `[patch]` set already used by `scratch/hello`.
  3. Run 1: **14 errors, one cause, not API drift.** The `IntoPlot` derive (charts) locates the gpui crate with `proc_macro_crate::crate_name` over `["gpui-kit","gpui-pre","gpui-fast"]` (`gc/gpui-component-macros-0.11.0/src/crate_path.rs:7-15`). Our dependency is package `gpui`, so 7 `IntoPlot requires a direct dependency…` errors appeared, plus 7 follow-on `IntoElement not satisfied`.
  4. Fix: add `"gpui"` to that list (one word, in the vendored copy).
  5. Run 2: **`cargo check` clean, 0 errors.** A tiny consumer (`src/main.rs`: `Input` + `Textarea` with `auto_grow(1, 12)` inside `gpui_component::Root`) also type-checks.
  - So gpui-component 0.11.0 currently has **no API drift** against `a3ce8d5e`. Logs: `check-run1.log` (14 errors), `check.log` (clean).
  - It also **links and runs**: `examples/capture.rs` renders `Input` + `Textarea` headlessly, typing through the keymap and showing an IME preedit (§10.5).
- **Other non-GPL options** (crates.io search for "gpui input/text/editor/textarea/widget", 2026-11-26):
  - Nothing else is comparable: `gpui-component` has ~700k recent downloads, and the next are niche.
  - `adabraka-ui` 0.3.9: MIT, last 2026-02-19, uses its own fork `adabraka-gpui ^0.5`, ropey 1.6.
  - `bezel-ui` 0.2.30: MIT, own fork `bezel-gpui`.
  - `guise-ui` 1.10.0: MIT, crates.io `gpui ^0.2.2`, which is a year stale.
  - `gpui-ce`: a community fork of gpui itself, not a widget set.
  - Each would need porting to our pin.

### 7.7 Recommendation

> **Survey lead's note (after the size/idle measurement in §7.6):** the whole `gpui-component` costs +10.4 MB of
> binary and brings an idle thread that wakes 2×/s, which conflicts with the client's resource goals. Take the
> *engine*, not the kit: vendor `gpui-base`'s `input/` (rope buffer, undo, movement, wrap, IME handler) at a pinned
> version into our tree, strip what we don't use (code editor, LSP, markdown, i18n, file watching), and render it
> with our own design tokens. Re-measure binary size and idle wakeups after the trim; if the trimmed engine is
> still heavy, fall back to writing the single-line field ourselves (small) and keep only the textarea engine.

- **Adopt the input engine from gpui-component / gpui-base, vendored at a pinned version. Do not write our own composer from scratch.** Reasons:
  - It is Apache-2.0, so licence-compatible.
  - It already builds against our exact Zed pin with zero API drift (§7.6). Its gpui-pre snapshot is only 4 days and 79 commits behind ours.
  - It has the hard parts we would otherwise spend weeks on: rope buffer, UTF-16/IME, soft wrap, auto-grow, undo coalescing, word/grapheme movement, Enter/Shift+Enter submit semantics, `ctrl-*` bindings for Linux/Windows, a11y value.
  - The dependency cost is moderate (+~74 crates, no tree-sitter by default).
- **How to consume it:**
  - Option (a), simplest: switch our gpui dependency to the same crates.io snapshot (`gpui-pre =0.3.14`, `gpui-pre-platform`), so plain `gpui-component = "=0.11.0"` works unmodified, and move both together.
  - Option (b): keep the Zed git pin and vendor `gpui-base` + `gpui-component` (+ macros + assets) with rewritten manifests plus the one-word `crate_path.rs` patch, as in the check dir. Re-run the check on every Zed bump.
  - Either way, pin `=` versions. Upstream breaks API roughly weekly.
  - We may vendor only `gpui-base` (engine) + the `Input`/`Textarea` modules and style them with our tokens, because the components bring their own theme system (`gpui_component::init`, `Root`).
- **What writing our own would take**, for comparison (all on GPUI APIs cited above):
  - Single-line field (search, login, settings): `String`, `unicode-segmentation` grapheme + word boundaries, anchor/head selection, mouse drag + double/triple click, clipboard + Linux primary, `EntityInputHandler` with UTF-16 maps, horizontal scroll-into-view, placeholder/password/disabled, small snapshot undo. About 800–1,200 LoC + tests. Mostly mechanical, using `examples/input.rs` as a template.
  - Multi-line composer: add `shape_text(.., wrap_width, ..)` layout per paragraph, mapping positions over `WrappedLine` (`index_for_position`/`position_for_index`), up/down with goal-x, auto-grow height and a scroll container, IME candidate bounds across wrapped lines, Enter vs Shift+Enter, paste of multi-line text, transactional undo, and `invalidate_character_coordinates` on scroll. A `String` is adequate for chat-sized text (≤ 4–8 KB), so no rope is needed. About 2,500–4,000 LoC + tests.
  - Inline mention/emoji tokens later add more. gpui-base already has `InlineToken` support (`gc/gpui-component-0.11.0/src/input/mod.rs:6-10`).
  - Fallback if gpui-component churn becomes a problem: write the single-line field ourselves (small), and keep the vendored gpui-base `TextareaState` for the composer.

### Gaps and workarounds

- **Wayland:**
  - No `set_surrounding_text`/`delete_surrounding_text`.
  - Preedit cursor position and style are dropped (`new_selected_range` is always `None`).
  - Content purpose is always `Normal`, even for password fields.
  - One-byte commits are re-dispatched as key presses, so a binding on a plain letter in a parent context could swallow IME output.
  - Workaround: no `ctx`-free single-letter bindings while a field is focused. Patch `gpui_linux` only if a real IME misbehaves.
- **X11:** XIM depends on a running IM server (fcitx5/ibus with XIM). The candidate `SpotLocation` uses the bottom-right of the bounds. Without a server you get xkb compose only.
- **Windows:** IMM32 only (no TSF). Modern IMEs work through the IMM32 compatibility layer. Expect rougher edges with reconversion and touch keyboards.
- **Linux/Windows ignore** `text_input_state_changed` and `set_text_input_configuration` (default no-ops, `platform.rs:1328`, `1526`). Call the notify APIs anyway, for portability.
- **`show_character_palette`** is macOS-only (`platform.rs:1442`). On Linux/Windows the emoji picker must be our own UI.
- **gpui-component:**
  - Weekly breaking releases. It tracks gpui-pre snapshots, not Zed git, so vendoring needs a manifest rewrite.
  - The `IntoPlot` macro hard-codes crate names (patch `crate_path.rs:7` or use a gpui-pre dependency).
  - It carries its own theme and `Root` (needed for popovers and context menus in the input).
  - Default deps include `notify`, `rust-i18n`, `lsp-types`, `markdown`/html5ever even if unused.
- **No** live IME session was run (fcitx/ibus/Windows IME) against gpui-component or GPUI on this machine. IME behaviour above is from source. The marked-text rendering was exercised by calling `replace_and_mark_text_in_range` directly (§10.5).
- `examples/input.rs` keybindings are macOS-only (`cmd-*`). Copy with `ctrl-*` on Linux/Windows.

## 8. Images and vectors

All refs are to the Zed clone at `a3ce8d5e`, paths relative to the zed root. The scene primitive types (quads, shadows, paths, mono/subpixel/polychrome sprites, surfaces, native views) are defined in `crates/gpui/src/scene.rs:491-503` (`Primitive` enum), stored per scene in `crates/gpui/src/scene.rs:51-68`. Video and surfaces are covered in another section.

### 8.1 `img()` and `ImageSource`

- `img(source: impl Into<ImageSource>) -> Img`: `crates/gpui/src/elements/img.rs:195-204`.
- `enum ImageSource` (`crates/gpui/src/elements/img.rs:36-47`):
  - `Resource(Resource)`. `Resource` is `Uri(SharedUri) | Path(Arc<Path>) | Embedded(SharedString)` (`crates/gpui/src/asset_cache.rs:69-78`).
  - `Render(Arc<RenderImage>)`: pixels you already decoded. No cache lookup; it is returned as is (`crates/gpui/src/elements/img.rs:539`).
  - `Image(Arc<Image>)`: encoded bytes plus a format. GPUI decodes them and caches the result globally (`crates/gpui/src/elements/img.rs:540`).
  - `Custom(Arc<dyn Fn(&mut Window, &mut App) -> Option<Result<Arc<RenderImage>, ImageCacheError>>>)`. The closure runs on every layout and every paint (`crates/gpui/src/elements/img.rs:538`). Returning `None` means "still loading".
- Conversions:
  - `&str`, `String` and `SharedString` become `Uri` if `url::Url::from_str` accepts them, otherwise `Embedded` (an `AssetSource` path) (`crates/gpui/src/elements/img.rs:49-83`). Pass a `Path` or `PathBuf` for files. (Untested: on Windows a drive-letter string such as `C:\x.png` may parse as a URL with scheme `c`.)
  - `Path`, `PathBuf` and `Arc<Path>` become `Resource::Path` (`crates/gpui/src/elements/img.rs:85-101`).
  - `Arc<RenderImage>`, `Arc<Image>` and closures convert to their variants (`crates/gpui/src/elements/img.rs:103-122`).
- `StyledImage` methods (`crates/gpui/src/elements/img.rs:143-185`; also implemented for `Stateful<Img>`):
  - `.grayscale(bool)`
  - `.object_fit(ObjectFit)`, default `Contain` (`crates/gpui/src/elements/img.rs:132-141`)
  - `.with_fallback(|| AnyElement)`, shown on error (`crates/gpui/src/elements/img.rs:386-395`)
  - `.with_loading(|| AnyElement)`, shown only after `LOADING_DELAY` = 200 ms (`crates/gpui/src/elements/img.rs:27-28, 396-417`)
- `Img` implements `Styled`, `InteractiveElement` and `StatefulInteractiveElement` (`crates/gpui/src/elements/img.rs:509-521`). Size, rounding, border, opacity, hover, click and tooltip all work on it.
- Intrinsic sizing:
  - Without an explicit aspect ratio, `img` takes the image's ratio (`crates/gpui/src/elements/img.rs:344-348`).
  - An `auto` width or height comes from the image's pixel size divided by `RenderImage::scale_factor` (`crates/gpui/src/elements/img.rs:350-376`; `crates/gpui/src/assets.rs:89-93`).
- Cache selection, in order (`crates/gpui/src/elements/img.rs:216-231, 304-308, 530-537`):
  1. the element's own `.image_cache(&Entity<I: ImageCache>)`;
  2. the nearest `image_cache(...)` ancestor (`window.image_cache_stack`);
  3. otherwise the global asset cache.
- `Img::extensions()` lists `avif` (`crates/gpui/src/elements/img.rs:207-214`), but AVIF decoding is **not** compiled in (`Cargo.toml:669-670`).

### 8.2 How a resource is loaded (URL / file / embedded)

- `ImageAssetLoader::load` (`crates/gpui/src/elements/img.rs:603-672`):
  - `Path`: `fs::read` (`crates/gpui/src/elements/img.rs:622`).
  - `Uri`: `cx.http_client().get(uri, empty body, follow_redirects = true)`. The whole body is read into a `Vec` with **no size cap** and no custom headers. A non-2xx status becomes `ImageCacheError::BadStatus` (`crates/gpui/src/elements/img.rs:623-644`).
  - `Embedded`: `AssetSource::load(path)`; a missing asset gives `ImageCacheError::Asset` (`crates/gpui/src/elements/img.rs:645-654`).
  - Then `image::guess_format`. Bytes it doesn't recognise are treated as SVG and rasterised once at scale 1 (`crates/gpui/src/elements/img.rs:657-669`).
- The load future runs on GPUI's **background executor** (`CachedLoad::new` spawns it there, `crates/gpui/src/asset_cache.rs:25-49`). Fetch and decode stay off the UI thread. Completion notifies every view that asked while it was loading (`crates/gpui/src/asset_cache.rs:36-46, 58-66`).
- HTTP client:
  - The default is `NullHttpClient`, which fails with "No HttpClient available" (`crates/gpui/src/app.rs:242-248, 3767-3790`).
  - Provide one with `Application::with_http_client(Arc<dyn HttpClient>)` (`crates/gpui/src/app.rs:281-285`) or `App::set_http_client` (`crates/gpui/src/app.rs:2246-2249`).
  - The trait is `http_client::HttpClient`: `user_agent()`, `proxy()`, `send(http::Request<AsyncBody>) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>>`, with `get()` provided (`crates/http_client/src/http_client.rs:117-146`).
  - Ready-made implementation: `reqwest_client::ReqwestClient` (`crates/reqwest_client/src/reqwest_client.rs:21-57`). It uses the current tokio runtime handle, or lazily builds its own multi-thread runtime (`reqwest_client.rs:123-139`). The gpui `image` example wires it this way (`crates/gpui/examples/image/image.rs:157-163`).
- For Decibell: URL loads have no size cap and no private-network guard. Don't hand untrusted URLs (link previews, avatars from peers) to `img()`. Fetch them in our own tokio code with our caps and pass a `RenderImage` (§8.7).

### 8.3 Decoding

- `image` 0.25 features enabled: `bmp, dds, exr, ff, gif, hdr, ico, jpeg, png, pnm, qoi, rayon, tga, tiff, webp` (`Cargo.toml:671-687`). No AVIF: the comment says it would need `avif-native` (`Cargo.toml:669-670`).
- Static images (`crates/gpui/src/platform.rs:3462-3487`): decoded at **full resolution**, EXIF orientation applied, converted to RGBA8, then R and B swapped to BGRA.
  - There is no decode-at-target-size or downscale step anywhere.
  - The atlas receives the decoded size (`crates/gpui/src/window.rs:5598-5609`).
- GIF: **every frame decoded up front** into memory (`platform.rs:3489-3520`).
- Animated WebP: same as GIF, with a transparent background; static WebP takes the static path (`platform.rs:3522-3531`).
- APNG: `guess_format` returns PNG, so the static path decodes and shows only the first frame (`crates/gpui/src/elements/img.rs:657-662`; `platform.rs:3462-3470`).
- `RenderImage` (`crates/gpui/src/assets.rs:42-107`):
  - Holds `id: ImageId` from a global counter, a private `scale_factor` (default 1) and `data: SmallVec<[image::Frame; 1]>`.
  - Pixels are **BGRA with straight (non-premultiplied) alpha**. The shader premultiplies at blend time (`crates/gpui_wgpu/src/shaders.wgsl:388-392`); SVG output is un-premultiplied for this reason (`crates/gpui/src/color.rs:26-34`).
  - API: `RenderImage::new(impl Into<SmallVec<[Frame;1]>>)`, `size(i)`, `delay(i)` (default 100 ms), `frame_count()`, `as_bytes(i)`.
- `Image { format: ImageFormat, bytes: Vec<u8>, id: u64 }`. `Image::from_bytes` sets `id = hash(bytes)` (`platform.rs:3451-3460, 3545-3552`). It is decoded by `Image::to_image_data` (`platform.rs:3593-3612`) through the `ImageDecoder` asset (`crates/gpui/src/elements/img.rs:587-601`).
- `ImageFormat` variants: `Png, Jpeg, Webp, Gif, Svg, Bmp, Tiff, Ico, Pnm`, with `mime_type()`, `extension()` and `from_mime_type()` (`platform.rs:3374-3449`).
- gpui does **not** re-export the `image` crate (`crates/gpui/src/gpui.rs:116-153`). To build a `Frame`, depend on `image = "0.25"` yourself, the same major version gpui uses.

### 8.4 Image caches and memory behaviour

**Global asset cache**
- Lives in `App.loading_assets`, keyed by `(TypeId, hash(source))`. The doc says: "Pending loads and completed results are cached until `remove_asset`" (`crates/gpui/src/app.rs:3294-3313`).
- **No eviction and no size limit.** Every URL, path or `Arc<Image>` drawn without an `image_cache` keeps its decoded frames for the app's lifetime.
- `Window::use_asset` subscribes the view to completion; `get_asset` doesn't (`window.rs:4730-4746`).
- `ImageSource::remove_asset` and `Image::remove_asset` drop only the CPU entry (`crates/gpui/src/elements/img.rs:564-573`; `crates/gpui/src/app.rs:3280-3284`; `platform.rs:3581-3584`). The GPU tile stays until `drop_image`.

**`ImageCache` trait and the `image_cache` element**
- `trait ImageCache: 'static { fn load(&mut self, &Resource, &mut Window, &mut App) -> Option<Result<Arc<RenderImage>, ImageCacheError>>; }` (`crates/gpui/src/elements/image_cache.rs:187-198`). The trait doc says implementations must remove images from all windows when they no longer need them.
- `ImageCacheProvider` is implemented for any `Entity<T: ImageCache>` (`crates/gpui/src/elements/image_cache.rs:200-211`). `image_cache(provider)` is a container element that pushes the cache for its children during layout and paint (`crates/gpui/src/elements/image_cache.rs:12-20, 101-152`).
- `ImageCacheItem` is a public building block (`crates/gpui/src/elements/image_cache.rs:155-185`):
  - `::new(&Resource, cx)` starts the standard loader (http / fs / embedded) on the background executor.
  - `get()` returns the result without subscribing; `use_image(window)` subscribes.
  - Dropping the item cancels a pending load.
- `RetainAllImageCache` is a plain `HashMap` with **no eviction**, despite its doc comment calling it LRU (`crates/gpui/src/elements/image_cache.rs:213-214`).
  - `clear()` and `remove()` call `drop_image` (`crates/gpui/src/elements/image_cache.rs:257-274`).
  - When the entity is released, it drops every image from every window (`crates/gpui/src/elements/image_cache.rs:226-238`).
- `retain_all(id)` keeps a `RetainAllImageCache` in element state (`crates/gpui/src/elements/image_cache.rs:298-322`). Element state not accessed during a frame is discarded (`window.rs:1209-1216`). So the cache, its pixels and its atlas tiles are freed **when that subtree stops being rendered**. Wrapping one channel's message list in `image_cache(retain_all("chan-<id>"))` frees that channel's images on a channel switch.
- An LRU reference implementation is in `crates/gpui/examples/image_gallery.rs:136-235`. It calls `drop_image` on eviction.

**Memory per image**
- Decoded BGRA lives in the `Arc<RenderImage>`, and a copy lives in the per-window GPU atlas.
- The CPU copy cannot be released while the image is on screen: painting takes the `Arc<RenderImage>`, and its pixel data is private (`window.rs:5574-5609`; `crates/gpui/src/assets.rs:43-49`).
- An upload also stages a transient third copy until the next frame (`crates/gpui_wgpu/src/wgpu_atlas.rs:256-265, 381-392`).
- Example: a 1920×1080 frame is 7.9 MiB on the CPU plus 7.9 MiB on the GPU.

**Freeing an image**
- `cx.drop_image(image: Arc<RenderImage>, current_window: Option<&mut Window>)` removes every frame's tile from every window's atlas. Inside a window update, pass that window, because it is temporarily out of `App.windows` (`crates/gpui/src/app.rs:3370-3384`).
- `Window::drop_image(Arc<RenderImage>)` does the same for one window (`window.rs:5695-5707`).
- `RenderImage` has no `Drop` hook (searched `crates/gpui/src` for `impl Drop for RenderImage`: not found). Dropping the `Arc` without `drop_image` **leaks the atlas tile** for the window's lifetime.

### 8.5 Animated images (GIF / WebP; APNG plays as a still)

**How frames advance**
- Frames advance inside `Img::request_layout`, and only for an `img` with an element id (`.id(...)`). Without an id there is no element state, so the image stays on frame 0 and requests no frames (`crates/gpui/src/elements/img.rs:285-294, 378-384`).
- A frame advances once at least `delay(frame)` has elapsed, with carry-over, and by at most one frame per layout (`crates/gpui/src/elements/img.rs:315-333`).

**Cost while animating**
- While `frame_count > 1`, the window is active and reduced motion is off, `img` calls `window.request_animation_frame()` on **every** frame (`crates/gpui/src/elements/img.rs:378-384`).
- `request_animation_frame` schedules `cx.notify(current_view)` for the next frame (`window.rs:3090-3105`). The **whole owning view re-renders at display refresh rate**, not at the GIF's frame rate.
- Isolate each animated image in its own small view entity, optionally `AnyView::cached` / `Entity::cached` (`crates/gpui/src/view.rs:46, 258`).

**When it pauses**
- When the window loses OS focus: `is_window_active` means "focused by the OS" (`window.rs:3376-3379`; `crates/gpui/src/elements/img.rs:318-336`).
- When `cx.reduce_motion()` is set (`crates/gpui/src/elements/img.rs:317, 381`).

**Off-screen**
- It depends on whether the element is still in the rendered tree.
- Children of a plain `overflow_y_scroll()` div are always laid out; `div` doesn't cull by bounds (`crates/gpui/src/elements/div.rs:2445-2497`). A GIF scrolled out of such a container **keeps forcing frames**, and its tiles are still uploaded: the scene drops fully clipped primitives (`crates/gpui/src/scene.rs:225-229`) only after `paint_image` has inserted into the atlas (`window.rs:5598-5609`).
- `.invisible()` skips paint but not layout (`crates/gpui/src/elements/div.rs:2637-2639`), so it keeps ticking too.
- `uniform_list` and `list` render only the visible range (`crates/gpui/src/elements/uniform_list.rs:483-498`; `crates/gpui/src/elements/list.rs:855-910`). Off-screen GIFs there cost nothing.

**Memory**
- All frames are kept in CPU memory.
- Each frame becomes its own atlas tile keyed `(image_id, frame_index)` (`crates/gpui/src/assets.rs:35-40`; `window.rs:5593-5609`). After one loop every frame is also resident on the GPU.
- Example: 480×270 with 100 frames is about 49 MiB CPU + 49 MiB GPU.

**Recommendation for chat GIFs**
- Write our own element or cache:
  - decode at display size and cap the frame count;
  - advance on a timer at the frame delay, with a `render_at`-style wake instead of `request_animation_frame` (pattern in `crates/gpui/src/elements/animation.rs:240-265`);
  - render inside `list`/`uniform_list`;
  - `drop_image` when an image leaves the window.

### 8.6 ObjectFit, rounding, opacity, painting

- `ObjectFit::{Fill, Contain, Cover, ScaleDown, None}` (`crates/gpui/src/style.rs:28-139`). `Cover` crops by sampling a sub-rectangle of the tile; no extra copy (`window.rs:5613-5651`; test `crates/gpui/src/elements/img.rs:773-826`).
- **`rounded_*` on `img` works.**
  - The element's corner radii are passed to `paint_image` (`crates/gpui/src/elements/img.rs:490-499`) and clamped to the visible bounds (`window.rs:5654-5656`; test `crates/gpui/src/elements/img.rs:866-889`).
  - The polychrome sprite shader clips with an SDF: wgpu `crates/gpui_wgpu/src/shaders.wgsl:1449-1461`; DirectX `crates/gpui_windows/src/shaders.hlsl:1353-1369`.
  - `rounded_full()` gives circular avatars.
  - A rounded `overflow_hidden` **parent** does not round its children (§9.6).
- Background, border and shadow on an `img` are painted by its `Interactivity`, with the border on top (`crates/gpui/src/elements/div.rs:2646-2647`; `crates/gpui/src/style.rs:688-756`).
- Opacity becomes `PolychromeSprite.opacity` (`window.rs:5657-5667`). Grayscale is done in the shader (`crates/gpui_wgpu/src/shaders.wgsl:1456-1458`).
- `Window::paint_image(bounds, image_bounds, corner_radii, data: Arc<RenderImage>, frame_index, grayscale) -> Result<()>` is public (`window.rs:5566-5670`).
  - The visible region is `bounds ∩ image_bounds`, so pan and zoom need no re-upload.
  - It **panics on an invalid frame index** (`window.rs:5604-5605`).
  - Use it from `canvas()` for the image viewer's real-pixel zoom and minimap.
- Sampling is bilinear with **no mipmaps**: sampler at `crates/gpui_wgpu/src/wgpu_renderer.rs:1194-1197`, `mip_level_count: 1` at `crates/gpui_wgpu/src/wgpu_atlas.rs:211`, `MipLevels: 1` at `crates/gpui_windows/src/directx_atlas.rs:198`.
  - Downscaling a large photo several times into a thumbnail aliases, and it costs full-resolution VRAM.
  - Pre-scale to device size (CSS size × `window.scale_factor()`).

### 8.7 Images we decode ourselves (cheapest path, and freeing)

Fetch and decode in our own tokio runtime, or on `cx.background_executor()`, with caps. Resize to target device pixels there too. Then:

```rust
// off the UI thread
let mut buf: image::RgbaImage = decoded_and_resized;      // straight alpha
for px in buf.chunks_exact_mut(4) { px.swap(0, 2); }      // RGBA -> BGRA (as platform.rs:3481-3484)
let ri = Arc::new(RenderImage::new(SmallVec::from_elem(image::Frame::new(buf), 1))); // as img.rs:744-747
// send `ri` to the UI (Arc<RenderImage> is Send: Asset::Output must be Send, asset_cache.rs:104, and ImageDecoder's Output is Arc<RenderImage>, img.rs:592)
// UI: img(ri.clone()).size(px(w)).rounded_md()   -> ImageSource::Render, no cache (img.rs:103-107, 539)
// free: cx.drop_image(ri, Some(window)); then drop every Arc clone  (app.rs:3370-3384)
```

- Animated frames: build each `Frame` with the image crate's `Frame::from_parts(buf, 0, 0, Delay)`. gpui reads `frame.delay()` (`crates/gpui/src/assets.rs:95-101`). The `image` API itself was not checked in this tree.
- Other ways in:
  - `img(Arc::new(Image::from_bytes(ImageFormat::Png, bytes)))`. GPUI decodes on its background executor, but the result is cached **globally and forever**, keyed by a hash of the whole byte buffer (`platform.rs:3545-3552`; `crates/gpui/src/elements/img.rs:540, 587-601`). Fine for a few static assets; wrong for chat.
  - `ImageSource::Custom` closure over our own store. It is called on every layout and paint (`crates/gpui/src/elements/img.rs:538`); return `None` while loading, and `cx.notify` the view yourself on completion.
  - A custom `ImageCache` (trait above) to keep using `img("https://...")` resources with our own eviction.

### 8.8 `svg()` element

**Builder** (`crates/gpui/src/elements/svg.rs:18-86`)
- `.path(SharedString)` is resolved through `AssetSource` (`crates/gpui/src/svg_renderer.rs:313-319`).
- `.external_path(SharedString)` is a filesystem file, read asynchronously on the background executor and **cached forever** in the global asset cache (`svg.rs:194-199, 534-550`).
- `.data(&[u8])` hashes the bytes with `DefaultHasher` on every call to make a cache key `__binary_svg__{hash}` (`svg.rs:54-65`).
- `.with_transformation(Transformation)` and `.with_timed_transformation(TimedTransformation)` (see below).

**Sizing**
- **No intrinsic size**: layout comes only from style (`svg.rs:101-116`). Always size it, e.g. `.size_4()`.
- A zero size fails with "can't render at a zero size" (`crates/gpui/src/svg_renderer.rs:294`). A missing `.path` asset paints nothing (`crates/gpui/src/svg_renderer.rs:317-318`; `window.rs:5518-5529`).

**Monochrome only**
- The SVG is rasterised to an **alpha mask** (`crates/gpui/src/svg_renderer.rs:289-311`) in a Monochrome atlas texture (`platform.rs:1862`), then tinted.
- The tint is the element's **own** `text_color`. With no `.text_color(...)` on the `svg` itself **nothing is painted**, and the color is **not inherited** from the parent's text style: paint reads the element's computed `style.text.color` (`svg.rs:177-224`).
- `.hover(|s| s.text_color(..))` works.
- For multi-colour SVGs use `img("icons/x.svg")` (Embedded) or `img(path)`.
  - It is rasterised once at scale 1 × `SMOOTH_SVG_SCALE_FACTOR` and not re-rasterised when the size changes (`crates/gpui/src/elements/img.rs:616-618, 665-668`; `crates/gpui/src/svg_renderer.rs:252-287`).
  - It goes blurry above its nominal size; rasterise it yourself with `SvgRenderer::render_parsed(&parsed, SvgSize::Size(..))` (`crates/gpui/src/svg_renderer.rs:224-277`) for a crisp `RenderImage`.

**Rasterisation size**
- Snapped device-pixel bounds × `SMOOTH_SVG_SCALE_FACTOR` (= 2), rounded up. The aspect ratio follows from the width (`window.rs:5509-5516`; `crates/gpui/src/svg_renderer.rs:83-84, 333-338`).
- Capped at 8192 px (`crates/gpui/src/svg_renderer.rs:328-351`). Drawn centred at half the raster size (`window.rs:5530-5544`).
- It is per device pixel size: moving to a display with another scale factor rasterises again.

**Caching**
- Atlas key: `RenderSvgParams { path, size }` (`crates/gpui/src/svg_renderer.rs:86-91`; `window.rs:5511-5526`).
- On a miss, `.path` bytes are reloaded from `AssetSource`, parsed with `usvg` and rasterised with `resvg` **synchronously on the UI thread inside paint** (`crates/gpui/src/svg_renderer.rs:313-325`). The parsed tree is not kept.
- Each distinct size is a separate tile, and **SVG tiles are never evicted**: the only atlas `remove` call site is the image path at `window.rs:5703`.
- Don't animate an SVG's layout size, because every intermediate size leaks a tile. Use a timed transformation.
- SVG `<text>` resolves fonts through a lazily built copy of the system font database plus bundled fonts. Icon-only SVGs never build it (`crates/gpui/src/svg_renderer.rs:141-168`).

**Transforms and animation**
- `Transformation::{scale, translate, rotate, alpha}`; these don't affect layout or hit testing (`svg.rs:67-72, 242-337`).
- `TimedTransformation::new(period).from(a).to(b)` with per-property `Easing::{Linear, Quadratic, EaseInOut, EaseOutQuint, Heartbeat}`, plus `.alternate()`, `.with_max_fps(f)` and `::rotation(period, from)` (`svg.rs:339-523`).
- It animates **in the scene without re-rendering the view** (§9.11).
- With reduced motion it stays at `from` (`svg.rs:160-166`).

### 8.9 `AssetSource` and `with_assets`

- `trait AssetSource: 'static + Send + Sync { fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>>; fn list(&self, path: &str) -> Result<Vec<SharedString>>; }` (`crates/gpui/src/assets.rs:12-19`). `()` is the empty source (`crates/gpui/src/assets.rs:21-29`).
- `Application::with_assets(impl AssetSource)` sets the source and rebuilds the `SvgRenderer` around it (`crates/gpui/src/app.rs:265-273`). The default is `()` (`crates/gpui/src/app.rs:242-248`). Read it back with `cx.asset_source()` (`crates/gpui/src/app.rs:2644`).
- Consumers:
  - `svg().path()`: synchronous, on the UI thread, once per new raster size.
  - `img("relative/path")` (`Resource::Embedded`): on the background executor (`crates/gpui/src/elements/img.rs:645-654`).
  - Bundled fonts for SVG text (`crates/gpui/src/svg_renderer.rs:160`).
- Typical setup: `rust-embed` returning `Cow::Borrowed` static bytes. That costs no copy on load; `img` then copies once into a `Vec` (`crates/gpui/src/elements/img.rs:646-648`).

### 8.10 The sprite atlas (gpui_wgpu on Linux, DirectX on Windows)

- **One atlas per window**: each renderer builds its own (`crates/gpui_wgpu/src/wgpu_renderer.rs:351, 384`; `crates/gpui_windows/src/directx_renderer.rs:165`). An image shown in two windows is uploaded twice.
- Key → kind (`platform.rs:1841-1866`):
  - Glyph → Monochrome, Subpixel, or Polychrome for emoji;
  - Svg → Monochrome;
  - Image → Polychrome.
- Formats:
  - wgpu: `R8Unorm` for monochrome; the surface's colour format (BGRA8 or RGBA8, swizzled on upload) for subpixel and polychrome (`crates/gpui_wgpu/src/wgpu_atlas.rs:199-202, 381-392`).
  - DirectX: `R8_UNORM` / `B8G8R8A8_UNORM` / `R8G8B8A8_UNORM` (`crates/gpui_windows/src/directx_atlas.rs:175-194`).
- Sizes and growth:
  - A new texture is `max(1024×1024, tile size)`, capped at the device maximum. wgpu uses the adapter limit via `using_resolution` (`crates/gpui_wgpu/src/wgpu_atlas.rs:53, 183-198`; `crates/gpui_wgpu/src/wgpu_context.rs:455-462`); DirectX caps at 16384 (`crates/gpui_windows/src/directx_atlas.rs:164-174`).
  - Textures never grow: allocation tries existing textures newest first with etagere's `BucketedAtlasAllocator`, otherwise adds a texture (`crates/gpui_wgpu/src/wgpu_atlas.rs:162-181, 232`; `crates/gpui_windows/src/directx_atlas.rs:134-157`).
  - A 1024² polychrome texture is 4 MiB; a monochrome one is 1 MiB.
- An image larger than the device maximum fails to allocate ("failed to allocate", `crates/gpui_wgpu/src/wgpu_atlas.rs:134`). `paint_image` then returns `Err`, which `img` only logs (`crates/gpui/src/elements/img.rs:492-500`). Downscale huge images first.
- **No eviction**: `AtlasState` keeps tiles until `remove(key)` (`platform.rs:1914-1971`). A texture is freed only when its **last** tile is removed (`crates/gpui_wgpu/src/wgpu_atlas.rs:139-158`; `crates/gpui_windows/src/directx_atlas.rs:108-130`), so one small surviving tile can pin a 4 MiB texture.
- `clear()` only runs after repeated GPU errors or device loss (`crates/gpui_wgpu/src/wgpu_renderer.rs:1119-1127`; `crates/gpui_wgpu/src/wgpu_atlas.rs:91-110`). Glyph and SVG tiles therefore accumulate for the window's lifetime; only images can be dropped (`window.rs:5695-5707`).
- Uploads are queued in CPU memory and flushed before the frame (`crates/gpui_wgpu/src/wgpu_atlas.rs:73-76, 256-298`).

### Gaps and workarounds

- **Unbounded global cache.** `img(url | path | Arc<Image>)` without an `image_cache` keeps decoded pixels forever (`crates/gpui/src/app.rs:3294-3313`). Workaround: wrap views in `image_cache(retain_all(scope_id))` or a custom LRU, or use `RenderImage` with our own store and `drop_image`.
- **No downscale or decode-at-size.** Thumbnails cost full-resolution CPU and GPU memory (`platform.rs:3472-3487`; `window.rs:5598-5609`). Workaround: decode and resize in our own code (e.g. `image::imageops` / `fast_image_resize`) to device pixels, then pass a `RenderImage`.
- **URL fetch has no size cap, headers or SSRF guard** (`crates/gpui/src/elements/img.rs:623-644`). Workaround: fetch in our native tokio layer with the existing private-network guard and caps.
- **Animated images re-render the whole owning view every vsync and keep all frames on CPU and GPU** (`crates/gpui/src/elements/img.rs:378-384`; `window.rs:3102-3105`). Workaround: a small view per GIF, rendered in `list`/`uniform_list`, plus a custom element that wakes at the frame delay, caps frames and drops them when off screen.
- **`img` without `.id()` never animates** (`crates/gpui/src/elements/img.rs:378-384`). Workaround: give animated images a stable id.
- **No APNG or AVIF** (`crates/gpui/src/elements/img.rs:657-662`; `Cargo.toml:669-670`). Workaround: decode them ourselves (e.g. the `png` crate's APNG support, `dav1d`) into `RenderImage` frames.
- **Forgotten `drop_image` leaks atlas tiles.** `RenderImage` has no `Drop` hook. Workaround: one owner type per image whose `Drop` hands the image to a "to drop" queue processed with `cx.drop_image`.
- **Monochrome-only `svg()`; multi-colour SVG through `img` doesn't re-rasterise per size** (`crates/gpui/src/svg_renderer.rs:289-311`; `crates/gpui/src/elements/img.rs:616-618`). Workaround: keep monochrome icons on `svg()` and rasterise colour art at the needed size with `SvgRenderer::render_parsed`.
- **`svg()` paints nothing without its own `text_color`** (`svg.rs:177-224`). Workaround: an `Icon` helper that always sets `.text_color`.
- **SVG and glyph atlas tiles are never evicted; animating an SVG's size leaks a tile per size** (`window.rs:5511-5526`). Workaround: fixed icon sizes and timed transformations for motion.
- **Separate atlas per window** (`crates/gpui_wgpu/src/wgpu_renderer.rs:351`). Workaround: few windows; the pop-out stream player is video, which is covered elsewhere.

## 9. Styling primitives

### 9.1 Style model

- `Style` (`crates/gpui/src/style.rs:177-322`) derives `Refineable`. `StyleRefinement` holds all-`Option` fields.
- Every `Styled` element exposes `fn style(&mut self) -> &mut StyleRefinement` (`crates/gpui/src/styled.rs:20-34`).
- Tailwind-like helpers are generated by macros:
  - `style_helpers!`, `visibility_style_methods!`, `margin_style_methods!`, `padding_style_methods!`, `position_style_methods!`, `overflow_style_methods!`, `cursor_style_methods!`, `border_style_methods!`, `box_shadow_style_methods!` (`crates/gpui/src/styled.rs:26-34`);
  - defined in `crates/gpui_macros/src/styles.rs`.
  - Every helper has a custom-value form (`w(px(240.))`, `rounded_tl(px(6.))`) as well as presets.
- `Style::default()` has `display: Block` (`crates/gpui/src/style.rs:773-775`); call `.flex()` for flexbox.
- Paint order inside an element (`crates/gpui/src/style.rs:688-756`): drop shadows, background, inset shadows, children, then **border on top**.

### 9.2 Corner radii and borders (vs CSS)

**Corner radii**
- Helpers: `rounded`, `rounded_t/b/l/r`, and per corner `rounded_tl/tr/bl/br` (`crates/gpui_macros/src/styles.rs:594-617, 1161-1226`).
- Presets: `none, xs, sm, md, lg, xl, 2xl, 3xl`, and `full` = `px(9999.)` (`crates/gpui_macros/src/styles.rs:1228-1276`).
- The type is `Corners<AbsoluteLength>` (`crates/gpui/src/style.rs:287-289`): **no percentage radii and no elliptical (x/y) radii**.
- Radii are clamped to the box at paint time (`crates/gpui/src/style.rs:706-709`).

**Borders**
- **Per-side widths**: `Edges<AbsoluteLength>` (`crates/gpui/src/style.rs:249-251`), set with `border`, `border_t/b/l/r/x/y` and width presets (`crates/gpui_macros/src/styles.rs:1278-1420`).
- **A single colour** for all sides (`crates/gpui/src/style.rs:281-282`; `border_color`, `crates/gpui_macros/src/styles.rs:368-376`).
- Style is `Solid | Dashed` (`crates/gpui/src/scene.rs:924-933`; `.border_dashed()`, `crates/gpui/src/styled.rs:499-503`). Dashes are fixed at 2× the width with a 1× gap (`crates/gpui_wgpu/src/shaders.wgsl:724-727`). There is no dotted, double, inset or outset.
- A visible border also insets the overflow clip by its widths (`crates/gpui/src/style.rs:652-660`).

### 9.3 Box shadows

- `BoxShadow { color: Hsla, offset: Point<Pixels>, blur_radius, spread_radius, inset: bool }`, built with `BoxShadow::new(x, y, color).blur_radius(..).spread_radius(..).inset()` (`crates/gpui/src/style.rs:347-393`).
- Several shadows per element via `.shadow(Vec<BoxShadow>)`, plus the Tailwind presets `shadow_2xs` through `shadow_2xl` and `shadow_none` (`crates/gpui/src/style.rs:291-292`; `crates/gpui_macros/src/styles.rs:384-492`).
- Drop shadows (`window.rs:5001-5041`):
  - follow the element's corner radii;
  - are clipped by the current content mask (an ancestor's `overflow_hidden` cuts them off);
  - leave 3× the blur as margin.
- **Inset shadows are supported**. They are drawn after the background, inside a hole whose radii shrink with the spread (`window.rs:5043-5086`).

### 9.4 Backgrounds and gradients

- `.bg(impl Into<Fill>)` (`crates/gpui/src/styled.rs:489-497`). `Fill` has only `Color(Background)` (`crates/gpui/src/style.rs:850-892`): no background images.
- `Background` kinds are `Solid | LinearGradient | PatternSlash | Checkerboard` (`crates/gpui/src/color.rs:743-750`).
- `linear_gradient(angle_deg, from: LinearColorStop, to: LinearColorStop)` with `linear_color_stop(color, percentage 0..1)` (`crates/gpui/src/color.rs:858-898`).
  - Angle 0 points to the top and increases clockwise, like CSS.
  - **Exactly two stops** (`colors: [LinearColorStop; 2]`, `crates/gpui/src/color.rs:779-787`; shader `crates/gpui_wgpu/src/shaders.wgsl:139-152, 444-486`).
  - `.color_space(ColorSpace::Srgb | Oklab)` (`crates/gpui/src/color.rs:752-765, 920-926`). The shader dithers (`crates/gpui_wgpu/src/shaders.wgsl:483-485`).
- No radial or conic gradients, no more than two stops, no gradient borders or text (`border_color` and text colour are plain `Hsla`).
- `pattern_slash(color, width, interval)` and `checkerboard(color, size)` (`crates/gpui/src/color.rs:826-848`).
- Paths (`canvas` + `PathBuilder`) can be filled with any `Background` (`window.rs:5248-5264`).

### 9.5 Opacity and visibility

- `.opacity(f32)` affects the element and its children (`crates/gpui/src/styled.rs:745-749`). It is **multiplied into each primitive's colour** as it is painted (`window.rs:4667-4683`; e.g. `paint_quad` `window.rs:5142-5150`, `paint_image` `window.rs:5657`).
- It is **not a group composite**: overlapping children at opacity < 1 show through each other, unlike CSS.
- `.invisible()` (`Visibility::Hidden`) keeps layout and skips paint (`crates/gpui_macros/src/styles.rs:50-70`; `crates/gpui/src/elements/div.rs:2637-2639`). Layout and animation frames still run.
- `Display::None` removes the element from layout (`crates/gpui/src/style.rs:1131-1141`).

### 9.6 Overflow and clipping (rounded corners)

- `overflow_hidden()`, `overflow_x_hidden()` and `overflow_y_hidden()` (`crates/gpui_macros/src/styles.rs:129-157`). `overflow_scroll()`, `overflow_x_scroll()` and `overflow_y_scroll()` are on `StatefulInteractiveElement`, so they need `.id()` (`crates/gpui/src/elements/div.rs:1568-1585`).
- `enum Overflow { Visible, Clip, Hidden, Scroll }` (`crates/gpui/src/style.rs:1208-1223`).
- The clip is **rectangular only**:
  - `ContentMask` holds just `bounds` (`window.rs:2548-2551`).
  - `overflow_mask` builds a rectangle inset by the borders (`crates/gpui/src/style.rs:636-685`).
  - Masks are intersected (`window.rs:4613-4633`).
- **`overflow_hidden` + `rounded_*` does not clip children to the rounded shape.** A child's background or image pokes out at the corners.

### 9.7 Transforms

- **No transforms on divs, quads or images.**
  - `TransformationMatrix` (2×2 matrix plus translation; `unit`, `translate`, `rotate`, `scale`, `compose`; `crates/gpui/src/scene.rs:935-1061`) exists only on `MonochromeSprite` and `SubpixelSprite` (`crates/gpui/src/scene.rs:1063-1074, 1128-1139`), i.e. SVGs and glyphs.
  - `PolychromeSprite` (images) has no transform field (`crates/gpui/src/scene.rs:1147-1159`).
- `svg().with_transformation(..)` provides scale, translate, rotate and alpha without moving layout or hitbox (`svg.rs:67-72, 325-337`).
- `PathBuilder::transform` / `translate` / `scale` / `rotate` apply at build time, on the CPU (`crates/gpui/src/path_builder.rs:244-292`).

### 9.8 Blur and backdrop blur

- **None for elements.**
  - Searched `style.rs`, `styled.rs`, `scene.rs` and `window.rs` for `blur(` and `backdrop`. The only hits are focus removal `Window::blur` (`window.rs:2784-2785`) and a path-tile winding field `backdrop` (`crates/gpui/src/scene.rs:1242`).
  - The only blur primitive is box-shadow blur.
- Window level: `WindowBackgroundAppearance::{Opaque, Transparent, Blurred}` (`platform.rs:2996-3011`). It depends on the compositor and can't be applied per element.

### 9.9 Cursor styles

- `enum CursorStyle` (`platform.rs:3113-3196`): `Arrow` (default), `IBeam`, `Crosshair`, `ClosedHand`, `OpenHand`, `PointingHand`, `ResizeLeft`, `ResizeRight`, `ResizeLeftRight`, `ResizeUp`, `ResizeDown`, `ResizeUpDown`, `ResizeUpLeftDownRight`, `ResizeUpRightDownLeft`, `ResizeColumn`, `ResizeRow`, `IBeamCursorForVerticalLayout`, `OperationNotAllowed`, `DragLink`, `DragCopy`, `ContextualMenu`.
- CSS-named helpers (`crates/gpui_macros/src/styles.rs:159-330`): `cursor(..)`, `cursor_default/pointer/text/move/not_allowed/context_menu/crosshair/vertical_text/alias/copy/no_drop/grab/grabbing`, and the `*_resize` family.
- Set per hitbox during paint (`window.rs:4580-4600`; `crates/gpui/src/elements/div.rs:2670-2678`).
- **No** wait, progress, help, zoom, none or custom image cursors.

### 9.10 Interaction states

**State styles**
- `.hover(|s| ..)` (`crates/gpui/src/elements/div.rs:865-873`).
- `.group("name")` (`crates/gpui/src/elements/div.rs:796-800`) with `.group_hover("name", |s| ..)` (`crates/gpui/src/elements/div.rs:875-886`).
- `.active(|s| ..)` and `.group_active(..)` need `.id()` (`crates/gpui/src/elements/div.rs:1607-1630`).
- `.focus(..)`, `.in_focus(..)` (≈ `:focus-within`) and `.focus_visible(..)` all require `.track_focus(&handle)`. `focus_visible` applies only after keyboard input (`crates/gpui/src/elements/div.rs:1318-1348, 3525-3544`).
- `.drag_over::<T>(|s, &T, w, cx| ..)` and `.group_drag_over::<T>(..)` (`crates/gpui/src/elements/div.rs:1237-1270`).

**Refinement order** (`crates/gpui/src/elements/div.rs:3515-3636`)
- base → in_focus → focus → focus_visible → group_hover → hover → drag-over → group_active → active.
- Hover styles are suppressed during a drag and after touch input (`crates/gpui/src/elements/div.rs:3546-3585`).

**Cost of a hover change**
- A hover transition notifies the view, which **re-renders the whole view** (`crates/gpui/src/elements/div.rs:2954-3002`; test `crates/gpui/src/elements/div.rs:4598-4639`).
- The listener that notifies keeps its hover flag in element state, which exists only for elements with an `.id()` (`crates/gpui/src/elements/div.rs:2445-2460`; `window.rs:4911-4936`). An id-less `.hover()` element may therefore not repaint on its own. This is inferred from the code (the Zed test relies on an id'd sibling), so give hover-styled elements an id.

**Not available**
- No `:disabled`, `:checked`, `:first-child` and so on. Use `.when(cond, |el| ..)`.

### 9.11 Transitions, animations, easing, reduced motion

**`with_animation`**
- There is no CSS `transition` property.
- `el.with_animation(id, Animation, |el, t| el..)` and `.with_animations(id, Vec<Animation>, |el, ix, t| ..)` (`crates/gpui/src/elements/animation.rs:96-131`).
- `Animation::new(Duration)` is one-shot and linear. Builders (`animation.rs:13-87`):
  - `.repeat()`;
  - `.repeat_synced()`: phase locked to an app-wide clock;
  - `.with_easing(impl Fn(f32) -> f32)`;
  - `.with_max_fps(f)`;
  - `.with_steps(n)`: like CSS `steps()`.

**Easing functions** (`animation.rs:541-600`)
- `linear`, `quadratic`, `ease_in_out`, `ease_out_quint()`, `heartbeat`, `bounce(f)`, `pulsating_between(min, max)`.
- **No cubic-bezier helper.** Pass your own closure to `with_easing`.

**Springs: the analogue of CSS transitions**
- `el.with_spring(id, SpringAnimation::new(SpringConfig::new(stiffness, damping, mass)).to(target).from(initial), |el, v| ..)` (`animation.rs:133-169, 276-379`; `crates/gpui/src/spring.rs:13-31, 452-540`).
- Changing `target` on a later render **retargets and keeps the velocity**.
- `SpringPlayback::{Running, Paused, Stopped, Completed, Cancelled}`.
- `sampled_easing(config, eps)` turns a spring into a duration-based easing (`crates/gpui/src/spring.rs:538`).
- `Interpolate` is implemented for `f32`, `Pixels`, `Rems`, `Rgba` and `Hsla` (`crates/gpui/src/spring.rs:408-450`).

**Cost**
- Every animated frame **re-renders the view that owns the element**: `request_animation_frame` notifies it (`animation.rs:496-508`; `window.rs:3102-3105`).
- `with_max_fps` and `with_steps` switch to timer wakes instead (`animation.rs:240-265, 496-506`).
- A finished one-shot stops requesting frames (`animation.rs:472-487`), so idle is still zero frames.

**Reduced motion**
- `App::reduce_motion()` and `set_reduce_motion(bool)` (`crates/gpui/src/app.rs:1434-1447`). The default is `false` (`crates/gpui/src/app.rs:1097`), and it is **not read from the OS**: searched `crates/gpui_linux`, `crates/gpui_windows` and `crates/gpui_platform` for `reduce_motion`, no hits. Wire the OS setting yourself.
- When it is set:
  - `with_animation` renders the end state of a one-shot or the start state of a repeating animation (`animation.rs:89-95, 433-440`);
  - springs snap to their target (`animation.rs:326-331`);
  - GIFs freeze (`crates/gpui/src/elements/img.rs:317`);
  - timed SVG transformations stay static (`svg.rs:160-166`).

**Scene animations (GPU side, no re-render)**
- Only `svg().with_timed_transformation(..)` uses them.
- The animated `MonochromeSprite` is moved before each present by `Scene::advance_animations` (`crates/gpui/src/scene.rs:89-129`; `window.rs:3107-3110`).
- The window keeps presenting, without redrawing, while the rendered scene has animations (`scene_animation_interval`, `window.rs:1293-1295, 1971-1997, 2068-2109, 3949-3950`).
- `with_max_fps` lowers the present rate (`svg.rs:468-476`; `crates/gpui/src/scene.rs:103-116`).
- A sprite that is fully clipped registers no animation (`crates/gpui/src/scene.rs:97-100`).
- This is the right tool for spinners and the speaking pulse on icons. It doesn't exist for quads or images.

### 9.12 Popovers, menus, tooltips, z-order

**`deferred`**
- `deferred(child).with_priority(n)` (`crates/gpui/src/elements/deferred.rs:6-29, 81-87`).
- Layout happens in place. Painting happens **after the whole tree**, sorted by priority, with **no ancestor clipping** (`content_mask` is `None`) (`crates/gpui/src/elements/deferred.rs:54-66`; `window.rs:4950-4982, 4183-4196, 4306-4437`).
- Nesting is limited to 10 levels (`window.rs:4326-4327`).
- Deferred draws carry the element-id and text-style stacks (`window.rs:4968-4981`).

**`anchored`**
- `anchored()` is an absolutely positioned flex box (`crates/gpui/src/elements/anchored.rs:124-146`).
- Builders:
  - `.anchor(Anchor)`;
  - `.position(Point)`: window coordinates by default, or `.position_mode(AnchoredPositionMode::Local)`;
  - `.attach(target_bounds, corner)`: flips to the other side of the target when that overflows less (`crates/gpui/src/elements/anchored.rs:186-231`);
  - `.offset(Point)`, which flips with it;
  - `.snap_to_window()` and `.snap_to_window_with_margin(edges)` (`crates/gpui/src/elements/anchored.rs:37-104`).
- The default fit mode is `SwitchAnchor` (`crates/gpui/src/elements/anchored.rs:232-257`). Every mode finally clamps to the window edges, counting the client inset (Linux CSD shadow) (`crates/gpui/src/elements/anchored.rs:259-282`).
- Popover idiom: `deferred(anchored().attach(trigger_bounds, Anchor::BottomLeft).child(menu)).with_priority(1)`.

**z-order**
- There is **no z-index**. Order comes from paint order (the scene's bounds tree puts later overlapping layers on top, `crates/gpui/src/scene.rs:157-174`), deferred priority, and `window.paint_layer(..)` (`window.rs:4984-4999`).
- Final overlays are exclusive: a prompt, else the active drag, else one tooltip (`window.rs:4154-4196`).

**Mouse blocking**
- `.occlude()` is `HitboxBehavior::BlockMouse`, which blocks hover, clicks and scroll behind the element. `.block_mouse_except_scroll()` lets scroll through (`crates/gpui/src/elements/div.rs:1294-1316`; `window.rs:902-954`).
- Elements with no listeners, cursor, hover or tooltip insert **no hitbox** (`crates/gpui/src/elements/div.rs:2500-2526`), so they are already transparent to the mouse (≈ `pointer-events: none`).

**Tooltips**
- `.tooltip(|window, cx| AnyView)` and `.hoverable_tooltip(..)` on id'd elements (`crates/gpui/src/elements/div.rs:1727-1749`), plus `.tooltip_show_delay(Duration)` (`crates/gpui/src/elements/div.rs:1751-1761`).
- Defaults: 500 ms to show, and 500 ms before a hoverable tooltip hides (`crates/gpui/src/elements/div.rs:53-54`).
- Placed at **mouse position + 1 px** and flipped at the window edges (`window.rs:4237-4282`).
- One at a time; not shown during a drag or prompt (`window.rs:4154-4175`).
- Discord-style tooltips anchored above the element have to be built from `on_hover` + `deferred(anchored().attach(..))`.

### 9.13 Drag and drop between elements

- `.on_drag(value: T, |&T, cursor_offset, window, cx| Entity<W: Render>)` on an id'd element (`crates/gpui/src/elements/div.rs:1660-1679, 633-659`).
  - Starts after a 2 px left-button move (`crates/gpui/src/elements/div.rs:52, 3083-3121`).
  - The dragged view is painted at mouse position minus the grab offset (`window.rs:4167-4172, 4190-4193`).
  - The **whole window refreshes on every mouse move** during a drag (`window.rs:6603-6613`).
- `.on_drop::<T>(|&T, w, cx| ..)` fires whether or not the drag started on this element (`crates/gpui/src/elements/div.rs:1272-1282`). `.can_drop(|&dyn Any, w, cx| bool)` filters (`crates/gpui/src/elements/div.rs:1284-1292`).
- `.drag_over::<T>` styles the target, `.on_drag_move::<T>` tracks position (`crates/gpui/src/elements/div.rs:1067-1078`), and `.external_drag_payload(..)` offers a payload when the drag leaves the window (`crates/gpui/src/elements/div.rs:1681-1696`).
- OS file drops arrive as `ExternalPaths` (`crates/gpui/src/interactive.rs:715`).
- During a drag the cursor is the source element's base cursor style (`crates/gpui/src/elements/div.rs:3004, 3115, 2670-2673`).

### 9.14 Text style inheritance

- Text properties live in `TextStyleRefinement` on any element (`crates/gpui/src/style.rs:294-296`).
- A div pushes its refinement for its subtree (`crates/gpui/src/elements/div.rs:2477, 2648`; `window.rs:4562-4578`). The effective style folds the stack (`window.rs:2833-2840`).
- `TextStyle` fields (`crates/gpui/src/style.rs:436-482`):
  - `color`, `font_family`, `font_features`, `font_fallbacks`, `font_size`, `line_height`, `font_weight`, `font_style`, `background_color`, `underline`, `strikethrough`;
  - `white_space`: `Normal | Nowrap`;
  - `text_overflow`: `Truncate | TruncateStart | TruncateMiddle`, with a custom ellipsis string;
  - `text_align`: `Left | Center | Right`;
  - `line_clamp`.
  - (`crates/gpui/src/style.rs:395-433`)
- Helpers: `text_color`, `text_size`, `text_xs…3xl`, `font_weight`, `italic`, `underline`, `line_through`, `text_decoration_*`, `truncate`, `text_ellipsis*`, `line_clamp`, `font_family`, `line_height` (`crates/gpui/src/styled.rs:75-150, 505-743`).
- Exception: an `svg` tint does **not** inherit (§8.8).
- **Missing**: letter-spacing, text-transform, text-shadow, justified text. Searched `style.rs` and `styled.rs` for `letter_spacing`, `text_transform` and `text_shadow`: not found. `TextAlign` has no `Justify` (`crates/gpui/src/style.rs:421-433`).

### 9.15 `when` / `map` / reusable styles

- `FluentBuilder` provides `map`, `when`, `when_else`, `when_some` and `when_none` (`crates/gpui/src/util.rs:10-62`). It is implemented for every `IntoElement` (`crates/gpui/src/element.rs:168`) and exported in the prelude (`crates/gpui/src/prelude.rs:8`).
- `refine_style`: **not found** (searched all of `crates/` for `fn refine_style`). Equivalents:
  - `StyleRefinement` is itself `Styled` (`crates/gpui/src/style.rs:324-328`). Build `let card = StyleRefinement::default().rounded_md().p_2();` once and apply it with `el.style().refine(&card)` (`Refineable::refine`, `crates/refineable/src/refineable.rs:29-36`).
  - Or use plain functions such as `fn card<E: Styled>(e: E) -> E`.

### 9.16 Layout primitives

- `Display::{Block, Flex, Grid, None}` (`crates/gpui/src/style.rs:1131-1141`).
- **Grid** (`crates/gpui/src/styled.rs:52, 751-889`; conversion in `crates/gpui/src/taffy.rs:842-866`):
  - `.grid()`;
  - `.grid_cols(n)` / `.grid_rows(n)` = `repeat(n, minmax(0, 1fr))`, plus `_min_content` (`minmax(min-content, 1fr)`) and `_max_content` (`minmax(0, max-content)`) variants;
  - placement with `col_start/end/span(_full)` and `row_*`.
  - **Equal tracks only**: no `72px 1fr 240px` track lists, named areas or `auto-fill`.
- **Absolute positioning**:
  - `.absolute()` / `.relative()` (`crates/gpui_macros/src/styles.rs:100-127`);
  - `inset`, `top`, `bottom`, `left`, `right` setters (`crates/gpui_macros/src/styles.rs:803-842`);
  - positions against the closest positioned ancestor (`crates/gpui/src/style.rs:1236-1247`). Every element defaults to `Relative` (`crates/gpui/src/style.rs:783`), so in practice that is the parent.
  - **No `fixed` or `sticky`.**
- Sizes: `w`, `h`, `size`, `min_w`, `min_h`, `min_size`, `max_w`, `max_h`, `max_size` (`crates/gpui_macros/src/styles.rs:843-905`). `.aspect_ratio(f32)` and `.aspect_square()` (`crates/gpui/src/styled.rs:475-487`).
- `container_query(|size, window, cx| element)` (`crates/gpui/src/elements/container_query.rs:1-49, 82-96`):
  - fills its parent by default;
  - renders its child in prepaint once its size is known;
  - the contents **cannot influence the element's own size**.
  - Fits the 1024 px breakpoints: settings icon rail, floating search panel. `window.viewport_size()` serves window-level breakpoints.

### Gaps and workarounds

- **Rounded clipping of children** (`window.rs:2548-2551`; `crates/gpui/src/style.rs:636-685`). Workarounds:
  - give the child the same radii: quads and images support per-corner radii natively, e.g. a header strip gets `rounded_t_*`, an image `rounded_*`;
  - or paint corner caps in the container's background colour on top, with a deferred or last-painted monochrome SVG.
- **Group opacity** (`window.rs:4667-4683`). Workarounds: fade only leaf content; avoid overlapping semi-transparent children; for a fading card, animate the background alpha and text colour alpha instead of `opacity`.
- **No transforms on divs or images** (`crates/gpui/src/scene.rs:1147-1159`). Workarounds:
  - hover-scale effects: animate size or padding with `with_spring` (this costs layout, keep it local);
  - rotation: pre-rotate pixels into a `RenderImage`;
  - icon motion: `svg().with_timed_transformation`.
- **Radial or conic gradients, more than two stops, gradient borders** (`crates/gpui/src/color.rs:779-787`). Workarounds: stack absolutely positioned two-stop layers with sub-range stops; or bake into a `RenderImage` at device size (dropped on theme change); or fill a path.
- **No `backdrop-filter`, `filter: blur()` or other filters** (§9.8). Workarounds: opaque or semi-opaque overlay backgrounds (design decision); grayscale exists only for images (`crates/gpui/src/elements/img.rs:148-152`).
- **Per-side border colours, dotted or double borders** (`crates/gpui/src/style.rs:281-285`). Workarounds: absolutely positioned 1 px edge divs; dashed is the only pattern.
- **No `outline` / `outline-offset`.** Workaround: a `focus_visible(|s| s.shadow(vec![BoxShadow::new(px(0.), px(0.), ring).spread_radius(px(2.))]))` ring; it is clipped by an `overflow_hidden` ancestor (`window.rs:5015`).
- **No CSS `transition`.** Workarounds: `with_spring` with a stable id for state-driven transitions; `with_animation` keyed by `(id, state)` to restart a one-shot.
- **No `cubic-bezier()`.** Workaround: a closure in `Animation::with_easing`.
- **Custom or missing cursors** (`platform.rs:3113-3196`). Workaround: none in gpui; pick the nearest variant.
- **No z-index.** Workarounds: paint order, `deferred(..).with_priority(n)`, `paint_layer`.
- **No `position: fixed/sticky`.** Workarounds: `deferred(anchored().position(..))` for fixed; for sticky date separators, render the header outside the scroll container and update its text from the scroll offset.
- **Grid track lists** (`crates/gpui/src/taffy.rs:842-866`). Workaround: nested flex with fixed-width children, as Discord's three-column layout already is.
- **No `mask` / `clip-path`** (status-dot cut-outs, avatar notches). Workarounds:
  - fake the cut-out with a ring in the surrounding background colour (border or a larger circle behind the dot);
  - or composite the avatar with its hole into a `RenderImage` on the CPU;
  - or draw the whole glyph as one monochrome SVG (matches the existing "SVG for tiny glyphs" rule).
- **No letter-spacing, text-transform or text-shadow.** Workarounds: uppercase the string in Rust; text shadow via a second, offset text layer; letter-spacing has no workaround short of per-glyph layout.
- **No media queries.** Workarounds: `container_query` or `window.viewport_size()`.
- **Element-anchored tooltips.** Workaround: build them from `on_hover` + `deferred(anchored().attach(bounds, ..))`; the built-in tooltip follows the mouse (`window.rs:4253-4254`).
- **No `background-image`.** Workaround: an absolutely positioned `img` behind the content.
- **Pseudo-classes beyond hover, active and focus.** Workaround: `.when(..)` and `.when_some(..)`.
- **Hover on an element without an id may not repaint** (`crates/gpui/src/elements/div.rs:2954-2978`). Workaround: give hover targets an `.id()`.
- **Each hover transition, drag move or animation frame re-renders the whole owning view** (`crates/gpui/src/elements/div.rs:2975`; `window.rs:6607`; `window.rs:3102-3105`). Workaround: split UI into small view entities and use `cached` views, so idle stays at zero frames and hot paths re-render little.

## 10. Testing and capture

### 10.1 `#[gpui::test]`, `TestAppContext`, deterministic executor

- `#[gpui::test]` (`crates/gpui_macros/src/gpui_macros.rs:151-191`; parsing in `crates/gpui_macros/src/test.rs:14-90`):
  - Wraps a sync or async fn into a plain `#[test]`, so `cargo test` and nextest work.
  - Parameters are injected by type name:
    - `&mut TestAppContext`: several allowed, one app each, sharing the dispatcher.
    - `StdRng`: seeded from the run's seed.
    - `BackgroundExecutor`.
  - The expansion calls `gpui::TestAppContext::build(dispatcher, Some(fn_name))` and `gpui::run_test(..)` (`test.rs:164`, `190`; `crates/gpui/src/test.rs:95`), so the consumer's dependency must be named `gpui`.
  - Arguments:
    - `seed = N`
    - `seeds(a, b, …)`
    - `iterations = N`: seeds `0..N`, combinable with seeds
    - `retries = N`
    - `on_failure = "path::fn"`
  - Env: `SEED` sets the first seed, `ITERATIONS` overrides `iterations` (`crates/gpui/src/seeds.rs:19-28`).
  - `PENDING_TRACES=1` captures traces of tasks that are still pending (`crates/gpui/src/platform/test/dispatcher.rs:29-30`).
- **Deterministic executor:** `TestDispatcher::new(seed)` wraps the `scheduler` crate's `TestScheduler` with `randomize_order: true, allow_parking: false` (`dispatcher.rs:24-34`). Foreground and background tasks all run on the test thread in seed-determined order. Main methods:
  - `run_until_parked()` = tick until no runnable work (`dispatcher.rs:76-78`).
  - `advance_clock(d)` (`:56`) for timers.
  - `allow_parking()`/`forbid_parking()` (`:80-86`) to block on real I/O.
  - `dispatch_after` panics: use `executor.timer()` (`:132-137`).
- **`TestAppContext`** (`crates/gpui/src/app/test_context.rs:35`):
  - Constructors: `build(dispatcher, fn_name)` with `TestPlatform` and `NoopTextSystem` (`:146`; `crates/gpui/src/platform/test/platform.rs:133-140`), `build_with_text_system(.., Arc<dyn PlatformTextSystem>)` (`:161`), `single()` (`:220`).
  - App access: `update`/`read` (`:285`, `:291`), `run_until_parked()` (`:624`), `dispatch_action(window, action)` (`:629`).
  - Input simulation:
    - `simulate_keystrokes(window, "ctrl-a backspace")`: space-separated keystrokes, then run until parked (`:646-657`).
    - `simulate_input(window, "hello")`: character by character (`:663-674`).
    - `dispatch_keystroke` (`:677`).
    - All three go through `Window::dispatch_keystroke`, so unhandled printable keys reach the focused `EntityInputHandler` (§7.2).
  - Other simulators: windows and displays (`:480-553`), clipboard (`:400-409`), prompts and paths (`:411-447`), system notifications (`:459-478`).
  - Platform windows: `test_window(handle)` is `pub(crate)` (`:685`). Out-of-tree tests cannot reach `TestWindow::text_input_state_changes()` etc. Test IME by calling your `EntityInputHandler` methods on the entity directly, as `crates/gpui/examples/input.rs:874-895` does.
- **Opening windows in tests:**
  - `add_window(|window, cx| V) -> WindowHandle<V>`: maximized on the 1-display test screen (`:298-315`).
  - `open_window(size, ..)` for an exact size (`:321-342`).
  - `add_window_view(|window, cx| V) -> (Entity<V>, &mut VisualTestContext)` (`:366-392`).
  - `add_empty_window()` (`:345-361`).
- **`VisualTestContext`** (`crates/gpui/src/app/test_context.rs:882`) derefs to `TestAppContext` and is bound to one window. It is unrelated to the macOS `VisualTestAppContext`.
  - Input: `simulate_keystrokes`/`simulate_input` (`:938-946`), `simulate_mouse_move`/`down`/`up`/`click` (`:949-1008`), `simulate_modifiers_change` (`:1011`), `simulate_event<E: InputEvent>` (`:1082-1086`, routed through `TestWindow::simulate_input`).
  - Window: `simulate_resize`, `simulate_scale_factor_change` (`:1027-1039`).
  - Inspection: `debug_bounds("selector")` (`:1045`, debug builds, with `.debug_selector(..)` on elements), `draw(origin, space, |w, cx| element)` (`:1050-1078`), `update(|window, cx| ..)` (`:892`).
  - `Window::painted_quads()` (`crates/gpui/src/window.rs:3285`, test-support) asserts on painted output without rasterizing.
- **Drawing in tests:** in `GpuiMode::Test`, every dirty window is drawn after effects flush (`crates/gpui/src/app.rs:2331-2344`). `skip_drawing()` turns this off (`crates/gpui/src/app/test_context.rs:215`).
  - `TestWindow` defaults to **scale factor 2.0** (`crates/gpui/src/platform/test/window.rs:144-145`).
  - The default `NoopTextSystem` lays text out monospaced: one em per char, two for emoji (`crates/gpui/src/platform.rs:1676`, `layout_line` ~`1749-1775`). Good for deterministic geometry, useless for pixels.
- **No display server needed.** `TestPlatform`/`TestWindow` never connect to one. The raw window and display handles return `HandleError::NotSupported` (`crates/gpui/src/platform/test/window.rs:73-89`). `cargo test` works over SSH or in CI without Xvfb.
- **Feature gating:** everything above needs `gpui` feature `test-support` in the consumer's dev-dependencies (`crates/gpui/src/app.rs:32-41`, `69-76`; `crates/gpui/src/gpui.rs:91-94`). `test-support` also enables gpui's `wayland`/`x11` features and proptest (`crates/gpui/Cargo.toml:21-31`). Pattern: `crates/gpui/examples/testing.rs:227-530` (counter tests, async, `allow_parking`, `iterations = 10` with `StdRng`, two-app interleaving).

### 10.2 Real headless platform and windowing modes (not for capture)

- `gpui_platform::headless()` = `Application::with_platform(current_platform(true))` (`crates/gpui_platform/src/gpui_platform.rs:20-22`). On Linux, `current_platform(headless)` → `linux_platform(WindowingModes::HEADLESS)`, also when `ZED_HEADLESS` is set (`crates/gpui_linux/src/linux.rs:40-46`).
- `gpui_platform::linux(allowed_modes)` (`crates/gpui_platform/src/gpui_platform.rs:29-35`), `Application::with_windowing(WindowingRequest)` (`crates/gpui/src/app.rs:302-305`) and `App::request_windowing(..) -> Task<Result<()>>` (`crates/gpui/src/app.rs:1391-1398`) switch between headless and windowed at runtime:
  - `WindowingRequest::{Headless, Windowed(GraphicalEnvironment)}` (`crates/gpui/src/platform.rs:310-315`).
  - `GraphicalEnvironment::detect()` reads `WAYLAND_DISPLAY`/`DISPLAY`/`XDG_RUNTIME_DIR` (`platform.rs:323-331`).
  - The switch fails while any window is open (`crates/gpui/src/app.rs:1391-1396`).
  - `crates/gpui/examples/toggle_headless.rs` drives this from a terminal REPL (create/ls/close/open).
- Real headless windows "lay out and handle input but draw nothing" (`platform.rs:311`). The Linux headless window's `draw` is empty (`crates/gpui_linux/src/linux/headless/window.rs:193`). **This mode cannot produce pixels.** It is for a client that keeps running with no display (e.g. a background daemon mode), not for screenshots.

### 10.3 Rendering a window to an image without showing it

- **Trait:** `PlatformHeadlessRenderer` (`crates/gpui/src/platform.rs:1553-1572`, cfg `test`/`test-support`/`bench-support`):
  - `render_scene_to_image(&mut self, &Scene, Size<DevicePixels>) -> Result<RgbaImage>`
  - `render_scene(..) -> Result<()>`
  - `sprite_atlas() -> Arc<dyn PlatformAtlas>`
- **Linux implementation: `gpui_wgpu::WgpuHeadlessRenderer`** (`crates/gpui_wgpu/src/wgpu_renderer.rs:2274-2515`), exported under `test-support`/`bench-support` (`crates/gpui_wgpu/src/gpui_wgpu.rs:10-14`; `crates/gpui_wgpu/Cargo.toml` `test-support = ["gpui/test-support", "dep:image"]`):
  - Renders into an offscreen `RENDER_ATTACHMENT|COPY_SRC` texture cleared to **black** (`:2330-2349`, `:2378`).
  - Reads back with `copy_texture_to_buffer` + `map_async`, strips row padding and swaps BGRA→RGBA (`:2384-2491`).
- **Device selection** (`crates/gpui_wgpu/src/wgpu_context.rs:188-257`):
  - Instance backends `VULKAN | GL` with **no display handle** (`:486-494`).
  - Enumerates all adapters and sorts them: `ZED_DEVICE_ID` override, discrete > integrated > other > virtual > CPU, Vulkan before GL (`:595-640`, `:327`).
  - Takes the first adapter that yields a device and a target format. A **CPU adapter (lavapipe/llvmpipe) is accepted**. Benches only log an error when one is chosen (`:230-237`).
  - One device is cached process-wide (`:188-204`).
  - So it **needs no compositor or X/Wayland**, only a Vulkan ICD or EGL/GL driver. On a headless CI box without a GPU, install Mesa lavapipe (`mesa-vulkan-drivers` / `vulkan-swrast`).
  - Unit tests that render quads and check pixels: `crates/gpui_wgpu/src/wgpu_renderer.rs:2638-2654`.
- **Wiring it to a GPUI window: `HeadlessAppContext`** (`crates/gpui/src/app/headless_app_context.rs`, cfg `test-support`, `crates/gpui/src/app.rs:32-33`, cross-platform):
  - `HeadlessAppContext::with_platform(text_system: Arc<dyn PlatformTextSystem>, assets: Arc<dyn AssetSource>, renderer_factory: impl Fn() -> Result<Option<Box<dyn PlatformHeadlessRenderer>>>)` (`:65-101`). It builds a `TestPlatform` with that factory (`crates/gpui/src/platform/test/platform.rs:207-268`) and a `TestDispatcher` seeded from `SEED`.
  - Each `open_window` calls the factory (`crates/gpui/src/platform/test/platform.rs:600-616`). The `TestWindow` uses the renderer's atlas, so glyphs and images rasterize into the right place (`crates/gpui/src/platform/test/window.rs:99-102`).
  - `open_window(size, |window, cx| Entity<V>)` opens with `show: false, focus: false` (`crates/gpui/src/app/headless_app_context.rs:104-126`).
  - `capture_screenshot(window) -> Result<RgbaImage>` = `window.render_to_image()` (`:177-180`), which re-renders the last frame's `Scene` (`crates/gpui/src/window.rs:3266-3271`; `crates/gpui/src/platform/test/window.rs:689-700`).
  - Also: `run_until_parked`, `advance_clock`, `allow_parking`, `simulate_window_scale_factor_change` (`:129-155`, `:134-140`), `update`/`update_window`.
  - There are no `simulate_*` key helpers. Use `update_window(h, |_, w, cx| w.dispatch_keystroke(Keystroke::parse("a")?, cx))`.
  - Real text needs a real text system: on Linux `gpui_wgpu::CosmicTextSystem::new(fallback_family)` or `::new_without_system_fonts(..)` plus `add_fonts` for reproducible pixels (`crates/gpui_wgpu/src/cosmic_text_system.rs:131-165`).
  - The factory to pass is `gpui_platform::current_headless_renderer` (`crates/gpui_platform/src/gpui_platform.rs:100-121`, feature `test-support` or `bench-support`): Linux → `WgpuHeadlessRenderer::new()`, macOS → Metal, **every other OS → `Ok(None)`**.
- **Other platforms:**
  - **Windows:** `current_headless_renderer()` is `Ok(None)` (`crates/gpui_platform/src/gpui_platform.rs:117-120`). There is no DirectX headless renderer, so `HeadlessAppContext` captures fail there with "no HeadlessRenderer configured" (`crates/gpui/src/platform/test/window.rs:698`).
    - What does exist: a real `WindowsWindow` implements `render_to_image` under `gpui_windows/test-support` (`crates/gpui_windows/src/window.rs:1044-1050` → `crates/gpui_windows/src/directx_renderer.rs:468-540`, staging-texture readback).
    - So on Windows you can capture a real (HWND-backed) window with `window.render_to_image()`. Open it with `WindowOptions { show: false, .. }` or off-screen bounds. This is untested here.
  - **macOS:** `VisualTestAppContext` + `VisualTestPlatform` (real Metal, off-screen at −10000,−10000) are macOS-only (`crates/gpui/src/app.rs:40-41`, `75-76`; `platform.rs:19-20`, `183-184`; `crates/gpui/src/app/visual_test_context.rs:14-19`, `385-387`). Not relevant to us.
  - **Real Linux Wayland/X11 windows** do not override `render_to_image` (grep in `crates/gpui_linux` finds none), so they use the default that bails (`platform.rs:1544-1550`).
  - Zed's own screenshot runner (`crates/zed/src/visual_test_runner.rs:701`) is GPL. Don't copy it.

### 10.4 Minimal sketch (out-of-tree, Linux)

```toml
[dev-dependencies]   # or a dedicated `capture` binary / feature
gpui          = { path = "…/zed/crates/gpui", default-features = false, features = ["test-support"] }
gpui_platform = { path = "…/zed/crates/gpui_platform", default-features = false, features = ["test-support"] }
gpui_wgpu     = { path = "…/zed/crates/gpui_wgpu" }          # CosmicTextSystem
image         = { version = "0.25", default-features = false, features = ["png"] }
```
```rust
use std::sync::Arc;
use gpui::{AppContext as _, HeadlessAppContext, Keystroke, px, size};

let fonts = Arc::new(gpui_wgpu::CosmicTextSystem::new("DejaVu Sans"));
let mut cx = HeadlessAppContext::with_platform(fonts, Arc::new(()), || {
    gpui_platform::current_headless_renderer()
});
let win = cx.open_window(size(px(480.), px(160.)), |window, cx| cx.new(|cx| MyView::new(window, cx)))?;
let any: gpui::AnyWindowHandle = win.into();
cx.simulate_window_scale_factor_change(any, 1.0);   // TestWindow defaults to 2.0
cx.update_window(any, |_, w, cx| { w.dispatch_keystroke(Keystroke::parse("a").unwrap(), cx); })?;
cx.run_until_parked();                              // test mode draws dirty windows here
cx.capture_screenshot(any)?.save("out.png")?;       // RgbaImage
```
- Inside `#[gpui::test]` the same works by building the context yourself. `TestAppContext` has no renderer-factory constructor, so use `HeadlessAppContext` or call `TestPlatform::with_platform(..)` directly.
- **Patch point if we want capture inside `#[gpui::test]`/`TestAppContext`:** add `TestAppContext::build_with_platform_parts(dispatcher, fn_name, text_system, renderer_factory)` that forwards to `TestPlatform::with_platform` (`crates/gpui/src/app/test_context.rs:161-207` already has the private `build_with_platform`). Also add a `capture_screenshot(window)` like `crates/gpui/src/app/headless_app_context.rs:177-180`. That is about 20 lines in `crates/gpui/src/app/test_context.rs`.
- **Patch point for Windows headless capture:** a `DirectXHeadlessRenderer` implementing `PlatformHeadlessRenderer` (an offscreen `ID3D11Texture2D` render target plus the existing staging readback, `crates/gpui_windows/src/directx_renderer.rs:468-540`) and a `cfg(target_os = "windows")` arm in `current_headless_renderer` (`crates/gpui_platform/src/gpui_platform.rs:100-121`). This is the larger option. The smaller one: capture real hidden windows via `render_to_image` on Windows only.

### 10.5 Verified on this machine

- Linux, KDE Wayland session (but none is used): `gpui-client/vendor/scratch/gpui-component-check/examples/capture.rs`. It renders gpui-component's `Input` + auto-growing `Textarea` in `HeadlessAppContext` with `current_headless_renderer()` + `CosmicTextSystem`, types through `dispatch_keystroke`, sets an IME-style marked run through `EntityInputHandler::replace_and_mark_text_in_range`, and saves a PNG.
- Build: `cargo build --example capture` took 3 m 34 s at 4 jobs (debug, with gpui `test-support`).
- **Runtime outcome:**
  - It prints `textarea value: "hello world\nxyz"` and `captured 480x160` at scale 1.0, and writes `capture-*.png` in that dir.
  - The image shows the "Search" placeholder input and a two-row auto-grown textarea with `hello world` / `xyz`, with `yz` underlined as preedit.
  - So gpui-component **runs** against our pin, its key bindings work (`space`, `shift-enter` → newline), and IME marked text renders.
- **Display independence:**
  - With `WAYLAND_DISPLAY`/`DISPLAY` unset, the PNG is byte-identical to the in-session one.
  - NVIDIA Vulkan (RTX 4080) vs **Mesa llvmpipe over EGL, no display** (`VK_ICD_FILENAMES=/nonexistent.json __EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json LIBGL_ALWAYS_SOFTWARE=1`): 77 pixels differ, max 1/255.
  - With Vulkan hidden but NVIDIA's EGL left as the vendor, `WgpuHeadlessRenderer::new()` failed: "No usable headless GPU adapter found". NVIDIA EGL found no device without a display.
  - lavapipe (Vulkan CPU) is not installed here, so it is untested.
- Windows: not run (no Windows box in this session). By source, `current_headless_renderer()` is `None` there (§10.3).

### Gaps and workarounds

- **Windows has no headless capture.** `current_headless_renderer()` → `None`. Run capture-based tests on Linux (CI), or capture real hidden windows via `render_to_image` with `gpui_windows/test-support` (untested). Pure-logic tests (`#[gpui::test]`, `NoopTextSystem`) run fine on Windows.
- **`TestAppContext` cannot take a renderer.** Use `HeadlessAppContext` for pixel tests (no `simulate_keystrokes` sugar; use `Window::dispatch_keystroke`), or patch in a constructor (§10.4, ~20 lines).
- **`test_window()` is `pub(crate)`.** Out-of-tree tests cannot read `text_input_state_changes`/`text_input_configurations` or take the installed input handler. Test IME via the entity's `EntityInputHandler` methods instead.
- **Determinism of pixels:**
  - Fonts: use `CosmicTextSystem::new_without_system_fonts` + bundled fonts.
  - Scale: set 1.0 explicitly (default 2.0).
  - The background clears to black (`crates/gpui_wgpu/src/wgpu_renderer.rs:2378`), so paint an opaque root.
  - Compare with a tolerance (lavapipe vs GPU raster differ slightly; the in-tree tests use ±3 per channel, `crates/gpui_wgpu/src/wgpu_renderer.rs:2617-2629`).
- **Real headless mode** (`gpui_platform::headless()`, `WindowingRequest::Headless`) draws nothing. It is not a capture path.
- **CI without a GPU** needs a Vulkan or GL software driver. Otherwise `WgpuHeadlessRenderer::new()` fails with "No usable headless GPU adapter found" (`crates/gpui_wgpu/src/wgpu_context.rs:256`).
  - Mesa llvmpipe through EGL is verified (§10.5). lavapipe should work by source but is untested.
  - On NVIDIA machines without a display, the Vulkan path works, but NVIDIA's EGL/GL path does not. Pin the software driver with the env vars in §10.5 for reproducible golden images.
- **`#[gpui::test]` expands to `gpui::…` paths.** Keep the dependency key `gpui` (renaming it breaks the macro).

## 11. Windows backend facts

Compile-level reading of `crates/gpui_windows` (cannot run here). `windows` crate 0.62 (`Cargo.toml:1028-1029`), with the feature list at `crates/gpui_windows/Cargo.toml:40-69`.

### 11.1 Crate features and build-time tools

- `gpui_windows` features: `default = ["gpui/default"]`, `test-support`, `screen-capture = ["gpui/screen-capture", "scap"]` (`crates/gpui_windows/Cargo.toml:15-18`, `:72-74`). `gpui` defaults `["font-kit","wayland","x11","windows-manifest"]` (`crates/gpui/Cargo.toml:20`); `windows-manifest = ["dep:embed-resource"]` (`:47`, `:203`). **`gpui_platform` force-enables `windows-manifest` on Windows** (`crates/gpui_platform/Cargo.toml:71`).
- Manifest: `crates/gpui/build.rs:31-46` compiles `resources/windows/gpui.rc` (`1 RT_MANIFEST "gpui.manifest.xml"`, `crates/gpui/resources/windows/gpui.rc:1-2`) with `embed_resource::compile(..).manifest_required()`. The manifest declares `asInvoker`, Windows 10 `supportedOS`, `dpiAware true/pm` + `dpiAwareness PerMonitorV2`, `SegmentHeap`, Common-Controls v6 (`crates/gpui/resources/windows/gpui.manifest.xml:5`, `:12`, `:17-19`, `:22-32`). Because `gpui` is lib-only, embed-resource 3.0.6 links the `.res` through `rustc-link-lib`, which propagates to our exe (`[embed-resource-3.0.6]/src/lib.rs:430-450`, crates.io source outside the zed tree). Needs `rc.exe` (MSVC) or `windres` (GNU). **Our own exe must not embed a second `RT_MANIFEST` id 1**; add only an `ICON` with **resource ID 1**, which GPUI loads for windows/taskbar (`crates/gpui_windows/src/platform.rs:1702-1716`, used in `register_window_class` `crates/gpui_windows/src/window.rs:1426-1436`).
- **HLSL shaders:** release builds compile `src/shaders.hlsl` (+ `alpha_correction.hlsl`) at build time with **`fxc.exe`** (`vs_4_1`/`ps_4_1`, `/O3`) into `OUT_DIR/shaders_bytes.rs` (`crates/gpui_windows/build.rs:3-12`, `:23-60`, `:138-195`; `include!` at `crates/gpui_windows/src/directx_renderer.rs:1886-1887`). fxc is found via `GPUI_FXC_PATH`, then `where.exe fxc.exe`, then the newest Windows 10 SDK in the registry; otherwise the build **panics** (`build.rs:104-128`, `:63-102`). The gate is `#[cfg(all(target_os = "windows", not(debug_assertions)))]` on the **build script's host** (`build.rs:14`, `:231`), so a **release cross-build from Linux has no shader blob and fails to compile**.
- **Debug builds compile shaders at runtime** with `D3DCompileFromFile` on the path `env!("CARGO_MANIFEST_DIR")/src/shaders.hlsl` (`crates/gpui_windows/src/directx_renderer.rs:1826-1884`). A debug `.exe` only runs where that vendor path exists, and needs `d3dcompiler_47.dll`.

### 11.2 Renderer and presentation

- **Direct3D 11 directly** (no wgpu on Windows): `D3D11CreateDevice` on the **first DXGI adapter that succeeds** (`EnumAdapters` order), feature levels 11_1/11_0/10_1, `BGRA_SUPPORT`, debug layer in debug builds. It requires StructuredBuffer support (`ComputeShaders_Plus_RawAndStructuredBuffers_Via_Shader_4_x`) (`crates/gpui_windows/src/directx_devices.rs:47-73`, `:106-194`). One device is shared by all windows (`crates/gpui_windows/src/platform.rs:309-324`, `:415-420`).
- **DirectComposition by default:** `CreateSwapChainForComposition` (B8G8R8A8_UNORM, **3 buffers**, `FLIP_SEQUENTIAL`, `ALPHA_MODE_PREMULTIPLIED`, `SCALING_STRETCH`) attached to an `IDCompositionTarget` per HWND; the window gets `WS_EX_NOREDIRECTIONBITMAP` (`crates/gpui_windows/src/directx_renderer.rs:28`, `:1037-1058`, `:1346-1370`, `:1724`; `crates/gpui_windows/src/window.rs:513-515`). Env `GPUI_DISABLE_DIRECT_COMPOSITION=1|true` switches to `CreateSwapChainForHwnd` with `ALPHA_MODE_IGNORE` + `DXGI_MWA_NO_ALT_ENTER` (no transparency) (`crates/gpui_windows/src/directx_renderer.rs:27`, `:1372-1401`; `crates/gpui_windows/src/platform.rs:254-255`).
- `Present(0, 0)`: sync interval 0, no vsync wait inside Present (`crates/gpui_windows/src/directx_renderer.rs:264-273`). Device-lost: the vsync thread checks `GetDeviceRemovedReason`, sleeps 350 ms, recreates devices (5 tries), and re-sends them to every window (`crates/gpui_windows/src/platform.rs:454-465`, `:1724-1733`, `:1778-1827`; `crates/gpui_windows/src/directx_devices.rs:24-36`). It panics if recovery fails (`crates/gpui_windows/src/platform.rs:463`).
- `gpu_specs()` is implemented (`crates/gpui_windows/src/window.rs:1060-1062`, `crates/gpui_windows/src/directx_renderer.rs:852`).

### 11.3 Frame pacing / vsync source

- A **`VSyncProvider` thread** loops on **`DwmFlush()`** (not `DCompositionWaitForCompositorClock`; that only appears in a comment) (`crates/gpui_windows/src/vsync.rs:31-60`). If `DwmFlush` fails or returns in under 1 ms (display asleep/unplugged) it sleeps one interval instead; the interval comes from `DwmGetCompositionTimingInfo` **once at thread start** (`crates/gpui_windows/src/vsync.rs:23-24`, `:32-36`, `:62-79`).
- Each vsync it calls `RedrawWindow(RDW_INVALIDATE)` on every tracked window and posts presentability changes (`crates/gpui_windows/src/platform.rs:432-500`). The resulting `WM_PAINT` → `draw_window` → GPUI `request_frame` (`crates/gpui_windows/src/events.rs:316-318`, `:1374-1425`), throttled by core (2.8). So all windows tick at the DWM (primary) rate.

### 11.4 Threads and message loop

- UI thread: `GetMessageW`/`TranslateMessage`/`DispatchMessageW`, with GPUI key handling first (`translate_accelerator`) (`crates/gpui_windows/src/platform.rs:589-611`, `:1199-1206`). A hidden top-level `"Zed::PlatformWindow"` (`WS_EX_TOOLWINDOW`) receives broadcasts (`WM_DISPLAYCHANGE`, `WM_ENDSESSION`, `WM_POWERBROADCAST`) and GPUI's private `WM_USER+n` messages (`crates/gpui_windows/src/platform.rs:217-235`, `:1221-1265`, `:1829-1838`; ids `crates/gpui_windows/src/events.rs:23-31`). App windows use class `"Zed::Window"` (`crates/gpui_windows/src/window.rs:1424`).
- Foreground executor: runnables are woken by a posted `WM_GPUI_TASK_DISPATCHED_ON_MAIN_THREAD` and run with a **10 ms budget**, then paint/input messages are drained before continuing (`crates/gpui_windows/src/platform.rs:1324-1388`).
- Threads started: `VSyncProvider` (`crates/gpui_windows/src/platform.rs:444-499`); Win32 **thread pool** for the background executor and timers (`TrySubmitThreadpoolCallback`, `CreateThreadpoolTimer`) (`crates/gpui_windows/src/dispatcher.rs:54-80`); `spawn_realtime` → a new std thread at `THREAD_PRIORITY_TIME_CRITICAL` (`crates/gpui_windows/src/dispatcher.rs:151-158`); one `windows-native-dialog` thread per file dialog / TaskDialog (`crates/gpui_windows/src/dialog.rs:113-115`); `scap` capture threads with `screen-capture`.

### 11.5 Text (DirectWrite)

- `IDWriteFactory5` (shared), in-memory font loader + font-set builder, **requires Windows 10 1703+** (`crates/gpui_windows/src/direct_write.rs:71-121`).
- `cx.text_system().add_fonts(Vec<Cow<[u8]>>)` (`crates/gpui/src/text_system.rs:302`) rebuilds a custom collection that is searched **before** the system collection (`crates/gpui_windows/src/direct_write.rs:248-284`, `:202-246`); the system collection is re-read once on a miss (`:227-242`).
- Fallbacks: the font's fallback list, then system fallbacks (`:286-341`). UI font = `SPI_GETICONTITLELOGFONT`, default "Segoe UI" (`:1730-1750`). ClearType vs grayscale follows `SPI_GETFONTSMOOTHINGTYPE` (`:188-198`, `:1713-1728`).
- **Colour emoji: only COLR layers are rasterized** (`// todo: support formats other than COLR`, `crates/gpui_windows/src/direct_write.rs:712-768`), although `is_color_glyph` also flags SVG/PNG/JPEG/BGRA glyphs (`:1756-1792`). Segoe UI Emoji (COLR) works; bitmap/SVG colour fonts would not render in colour.

### 11.6 IME

**IMM32**, not TSF. It uses `ImmSetCompositionWindow`/`ImmSetCandidateWindow` positioned from `update_ime_position`, `GCS_RESULTSTR`/`GCS_COMPSTR` parsing, and `ImmAssociateContextEx` to enable or disable per focused input handler (`crates/gpui_windows/src/events.rs:660-775`; `crates/gpui_windows/src/window.rs:1064-1073`). Searched `ITfThreadMgr|TextServices` in `gpui_windows`: not found.

### 11.7 DPI

- Process DPI awareness comes **only from the manifest** (PerMonitorV2). No `SetProcessDpiAwarenessContext`/`SetThreadDpiAwareness` call exists (searched `gpui_windows/src`: not found).
- Scale = `GetDpiForWindow / 96` (`crates/gpui_windows/src/window.rs:128-131`); `WM_DPICHANGED` applies the suggested rect, or re-fits a maximized window to the work area (`crates/gpui_windows/src/events.rs:875-947`).
- Logical coordinates are `f32` physical/scale (`crates/gpui_windows/src/util.rs:147-152`), so 125%/150% give fractional logical sizes. Snap chrome to whole device pixels. Frame metrics use `GetSystemMetricsForDpi` (`crates/gpui_windows/src/events.rs:1824-1834`).
- `WM_GETMINMAXINFO` converts `window_min_size` with the current scale (`crates/gpui_windows/src/events.rs:208-221`).

### 11.8 Re-entrancy and other behaviour Linux-developed code must expect

- **Modal size/move loop:** `WM_ENTERSIZEMOVE`/`WM_ENTERMENULOOP` start a `USER_TIMER_MINIMUM` timer. Inside the OS modal loop, foreground runnables and painting run **only from `WM_TIMER`** (`crates/gpui_windows/src/events.rs:279-314`).
- **Nested window procedures:** a shared `DrawCoordinator` defers re-entrant `WM_PAINT` and validates the rect (`crates/gpui_windows/src/events.rs:35-79`, `:1374-1388`); core skips re-entrant `request_frame` (`crates/gpui/src/window.rs:1929-1950`). Activation, visibility and display-change callbacks are deferred to the executor because the window procedure can run during a GPUI update (`crates/gpui_windows/src/events.rs:837-844`, `:960-974`, `:1304-1326`). Never assume a platform callback runs outside `App::update`.
- **`window.resize()` and `toggle_fullscreen()` are asynchronous** (executor-spawned `SetWindowPos`, `crates/gpui_windows/src/window.rs:304-365`, `:655-678`). `zoom`/`toggle_fullscreen` on a not-yet-shown window only record the initial state (`:947-965`).
- `WM_MOUSEACTIVATE` always returns `MA_ACTIVATE`, so clicks activate before dispatch (`crates/gpui_windows/src/events.rs:90-96`). Modifier state is resynced on activation (Alt-Tab) (`crates/gpui_windows/src/events.rs:819-835`). `activate()` injects an Alt key via `SendInput` (`crates/gpui_windows/src/window.rs:833-860`).
- Window destruction: hide immediately, `DestroyWindow` later on the executor after any owned dialog finishes (`crates/gpui_windows/src/window.rs:611-631`).
- Visibility can't see occlusion (a fully covered window keeps rendering) (`crates/gpui/src/platform.rs:97-100`).
- Precision-touchpad pan/zoom arrives through DirectManipulation (`crates/gpui_windows/src/direct_manipulation.rs:16-40`; `DM_POINTERHITTEST` `crates/gpui_windows/src/events.rs:159`).
- `GetAsyncKeyState` hotkey loops and GPUI coexist (GPUI uses message-based keys). Note GPUI's `translate_accelerator` consumes `WM_KEYDOWN`/`WM_SYSKEYDOWN` it handles (`crates/gpui_windows/src/platform.rs:1199-1206`).

### 11.9 Accessibility and high contrast (Windows)

- UIA via `accesskit_windows` 0.34 (`Cargo.lock:72-75`): adapter per HWND, served from `WM_GETOBJECT`, focus on `WM_ACTIVATE` (`crates/gpui_windows/src/window.rs:1080-1118`; `crates/gpui_windows/src/events.rs:800-811`, `:849-864`).
- High contrast / reduced motion from the OS: **not found** (searched `HighContrast|SPI_GETHIGHCONTRAST|ClientAreaAnimation` in `gpui_windows/src`).

### Gaps

- Release Windows builds need the Windows SDK `fxc.exe` on the build host; no Linux→Windows release cross-build; debug exes are tied to the source path.
- Single DXGI adapter = first enumerated; no GPU preference API.
- Vsync from `DwmFlush` on one thread with one interval read at start; no per-monitor or VRR-aware pacing; `Present` sync interval 0.
- IMM32 only (no TSF); colour emoji COLR only.
- No runtime DPI-awareness fallback without the manifest; must avoid a duplicate manifest in our exe.
- No `Floating`, `AnchoredPopup`, drag-out, CSD helpers (`start_window_move`, `set_client_inset`), or `set_app_id` on Windows.
- `hide_other_apps`/`unhide_other_apps` panic.
- No OS high-contrast or reduced-motion signal.

## 12. Gotchas, surprises, accessibility

### 12.1 Gotchas and surprises (collected from all sections)
1. **Zed's `[patch.crates-io]` is not inherited** by an out-of-tree app (async-task, calloop, khronos-egl must be repeated) — §1.4.
2. **crates.io `gpui` 0.2.2 is from 2025-10-22** and has the old API; depend on the pinned clone — §1.4.
3. **Idle wakeups depend on the backend**: Wayland parks completely; X11 wakes the main thread at the monitor refresh rate while the window is visible (280/s measured); Windows wakes at every vblank through `VSyncProvider` — §3.2, §13.
4. **wgpu enumerates the GL backend even when Vulkan works**, which loads every EGL vendor (Mesa `libgallium` + `libLLVM`, NVIDIA `libnvidia-eglcore`/`glcore`): +52 MB RSS, +16 MB Pss on this box (`crates/gpui_wgpu/src/wgpu_context.rs:486-494,531`). Avoid with a patch that enumerates GL only when Vulkan yields no usable adapter, or with `__EGL_VENDOR_LIBRARY_FILENAMES` — §13.
5. **The GL backend did not work at all here** (NVIDIA EGL: "no compatible surface formats" on Wayland and XWayland); a machine without working Vulkan gets no window. wgpu's `WGPU_*` env vars are ignored; only `ZED_DEVICE_ID` selects a GPU — §3.6.
6. **Discrete GPU preferred** after the compositor's GPU hint (`crates/gpui_wgpu/src/wgpu_context.rs:633-639`): on a hybrid laptop where the compositor runs on the iGPU the hint keeps us on the iGPU; without a hint (X11) the dGPU wins and may wake it.
7. **20 worker threads** (one per CPU) + `Timer` + smol/async-io + 3 zbus threads exist from startup; they are idle (0 wakeups) but count toward thread totals — §3.7.
8. **High-rate input keeps presenting for 1 s** after ≥6 invalidating inputs in 100 ms; **inactive windows are capped at 30 fps** while they have demand (`WindowOptions::inactive_frame_interval`) — §3.3. Set it to `None` for stream viewing in an unfocused window.
9. **Variable fonts render at their default instance on Linux** (no `wght`/`wdth`/`opsz` coordinates reach the rasteriser) and snap to named instances on Windows — §5.3. Instance fonts offline or patch `crates/gpui_wgpu/src/cosmic_text_system.rs:525-530`.
10. **Colour emoji on Linux only for a font whose PostScript name is `NotoColorEmoji`** (`crates/gpui_wgpu/src/cosmic_text_system.rs:1206`); fonts without an `m` glyph are rejected (`:421-428`) — §5.6.
11. **No inline elements in text**; selection across text blocks is ours to build — §5.6, §5.7.
12. **Images are cached forever** (asset cache, `RetainAllImageCache`, sprite atlas until `drop_image`), at full resolution on CPU *and* GPU — §8.4.
13. **An animated `img` with an id re-renders its view every frame** (not at the GIF's frame rate) while the window is active, and keeps doing so off screen unless it is inside a `list`/`uniform_list` that stops rendering it (`crates/gpui/src/elements/img.rs:378-384`) — §8.5.
14. **`svg()` is monochrome** (tinted by its own `text_color`; without one it paints nothing) and rasterised on the UI thread — §8.8.
15. **A rounded `overflow_hidden` parent does not clip children to its rounded corners** — §9.6.
16. **`list()` heights are exact only for rows that have been laid out**; rows outside viewport + overdraw count as 0 px or an estimate; with `ListAlignment::Bottom` pinned to the end, some queries report the item count as position — §6.2.
17. **`on_open_urls` never fires and `register_url_scheme` is unimplemented on Linux and Windows**; no tray, no badges, no single-instance — §2.12, §2.13.
18. **`request_attention()` is a no-op on Wayland**; `activate_window()` needs `WindowOptions::app_id` there — §2.9, §2.10.
19. **Windows release builds need `fxc.exe`** (Windows SDK) at build time (`crates/gpui_windows/build.rs:14-128`, override `GPUI_FXC_PATH`); cross-compiling a release build from Linux fails — §11.1.
20. **`cx.activate()` is log-only on Linux / no-op on Windows**; use `window.activate_window()` — §2.9.
21. **Keyring items on Linux are labelled `zed-github-account`** (`crates/gpui_linux/src/linux/platform.rs:57`, written at `:1095`, and reads/deletes filter on that label, `:1114,1142`): it works, but the label shows in the user's keyring UI. Use `oo7` directly if that matters — §2.18.
22. **NVIDIA driver threads wake 4.5/s on their own** (`[vkps] Update` 4/s + one unnamed 0.5/s) whatever GPUI does; `__GL_SHADER_DISK_CACHE=0` doesn't stop them and makes startup 2.5× slower — §13.
23. **System font scan is cheap**: cosmic-text `FontSystem::new()` indexes 1 095 faces in ~9 ms and ~1 MB RSS here; avoidable only by patching `crates/gpui_linux/src/linux/platform.rs:132` to `new_without_system_fonts` — §5.2, §13.

### 12.2 Accessibility

**What GPUI exposes**
- Guide: `crates/gpui/src/_accessibility.rs:1-292`; architecture notes: `crates/gpui/src/window/a11y.rs:1-98`; example `crates/gpui/examples/a11y.rs:66-223` (Application/Heading/SpinButton/Button/Switch/List/ListItem).
- Nodes exist only for elements with an **`.id()` and a non-`None` role**. `GenericContainer` is filtered out (`crates/gpui/src/elements/div.rs:1357-1364`, `crates/gpui/src/_accessibility.rs:58-66`). The AccessKit `NodeId` derives from the `GlobalElementId`, so ids must be unique and stable per frame (`crates/gpui/src/_accessibility.rs:55-103`, `a11y.rs:53-60`). A focused element without a node is logged once, and AT falls back to announcing the window (`a11y.rs:190-200`).
- Element API (`StatefulInteractiveElement`, `crates/gpui/src/elements/div.rs:1353-1560`): `role(accesskit::Role)`, `accessibility_id` (→ UIA AutomationId / AT-SPI id), and these `aria_*` setters: `label`, `description`, `keyshortcuts`, `active_descendant`, `selected`, `expanded`, `toggled`, `numeric_value(_step)`, `value`, `placeholder`, `min/max_numeric_value`, `orientation`, `level`, `position_in_set`, `size_of_set`, `row/column_index`, `row/column_count`. Also `on_a11y_action(accesskit::Action, ..)`, `a11y_synthetic_children(..)` and `focusable()`.
- Custom elements: `Element::a11y_role`, `write_a11y_info`, `a11y_synthetic_children` (`crates/gpui/src/element.rs:113-137`). Text from `text!()` / `Text` is a `Role::Label` node with `value` (`crates/gpui/src/elements/text.rs:181-189`, `:1173-1179`); `Text::new_inaccessible` opts out (`text.rs:92`).
- Window/app: `Application::new_inaccessible` (`crates/gpui/src/app.rs:250-263`), `window.is_a11y_enabled()`/`is_a11y_active()`/`debug_a11y_tree_json()` (`window.rs:7642`, `:7656`, `:7661`). The root node is `Role::Window` labelled with the title, which `set_window_title` keeps in sync (`window.rs:1764-1767`, `:3451-3454`).
- The tree is built only while an AT is connected, and pushed once per frame (`a11y.rs:131-145`, `:212`; `window.rs:1779-1883`, `:4232`).
- Built-in action fallback: `Click` (synthesized mouse down/up at the node centre), `Focus`, `Blur`; every other action needs an `on_a11y_action` handler, otherwise it is logged and dropped (`window.rs:7684-7744`). `on_click` auto-registers `Click` (`crates/gpui/src/_accessibility.rs:219-221`).

**Linux (AT-SPI)**
- `accesskit_unix` 0.22 (`Cargo.lock:54-57`), D-Bus/AT-SPI, enabled with either backend feature (`crates/gpui_linux/Cargo.toml:18-19`, `:36-37`, `:57`).
- Wayland: adapter created per window, focus state updated (`crates/gpui_linux/src/linux/wayland/window.rs:2282-2302`, `:1643`); `a11y_update_window_bounds` is a no-op ("Wayland doesn't expose window position", `:2304-2306`).
- X11: same, with a bounds implementation (`crates/gpui_linux/src/linux/x11/window.rs:2019-2069`, `:1376`). **But core never calls `a11y_update_window_bounds`** (the only occurrences are the trait default and the backend impls: `platform.rs:1537`, `crates/gpui_linux/src/linux/x11/window.rs:2041`, `crates/gpui_linux/src/linux/wayland/window.rs:2304`, `crates/gpui_windows/src/window.rs:1116`, `crates/gpui_macos/src/window.rs:2562`). So AT-SPI screen coordinates (highlight, hit-test) are likely wrong on X11 as well.

**Windows (UIA)**
- `accesskit_windows` 0.34 adapter served from `WM_GETOBJECT`; borrows are dropped before raising events to survive nested `WM_GETOBJECT` (`crates/gpui_windows/src/window.rs:1080-1118`; `events.rs:849-864`). UIA tracks window bounds itself (`window.rs:1116-1118`).

#### Gaps (accessibility)

- No live-region/announcement API (searched `set_live|Live::|aria_live|announce` in `crates/gpui/src`: not found). Chat "new message" announcements need another path.
- No OS high-contrast or reduced-motion detection (reduce-motion is app-set: `crates/gpui/src/app.rs:1434-1447`).
- Virtualized lists (`uniform_list`, `list`) expose only rendered rows (no a11y hooks in `elements/uniform_list.rs` / `elements/list.rs`).
- Actions beyond Click/Focus/Blur (ScrollIntoView, SetValue, SetTextSelection, …) must be implemented per element.
- AT-SPI window bounds never pushed on X11; impossible on Wayland.
- Text inputs must build their own text-run synthetic children for caret/selection reporting (`crates/gpui/src/_accessibility.rs:223-272`).

## 13. Measured hello-world

### 13.1 The app and how it was measured
- Source: `gpui-client/vendor/scratch/hello/` (`Cargo.toml`, `src/main.rs`, ~300 lines). One 1280×800 window,
  client-side decorations with a custom 36 px title bar (drag via `start_window_move`, min/max/close
  buttons with hover styles), and a bottom-aligned `list()` of 400 chat rows (name in a per-user colour,
  timestamp, 4–64 words of wrapping body text with an underlined link run via `StyledText::with_highlights`).
  System UI font (default fallback), 15 px / 22 px line height.
- Profiles: `release` = `opt-level=3, lto="fat", codegen-units=1, panic="abort", strip=true`; `release-s` =
  same with `opt-level="s"`; `release-thin` = `lto="thin"`. `CARGO_TARGET_DIR=gpui-client/vendor/scratch/target`.
  Variant binaries are kept in `gpui-client/vendor/scratch/bins/`. Clean release build: 3 min wall, 8.5 min CPU (8 jobs).
- Instrumentation in the app: `HELLO_T0_NS` (CLOCK_MONOTONIC taken by the launcher right before spawn) →
  marks printed at `main`, `app_run`, first root `render`, and the first `on_next_frame` callback
  (= the compositor's frame callback for the first present on Wayland, §3.4). Every root render bumps a
  counter printed on SIGUSR1 (async-signal-safe `write`). `HELLO_EXIT_AFTER_FIRST_FRAME=1` quits after the
  first frame. Since the root view is not cached, renders = drawn frames.
- Scripts: `gpui-client/vendor/scratch/measure.py` (`startup BIN N`: 1 discarded + N runs; `idle BIN SETTLE WIN`:
  settle 20 s, then `smaps_rollup`, per-thread `voluntary_ctxt_switches` and utime+stime over 30 s, render
  counter at both ends, `nvidia-smi --query-compute-apps`), `smaps_breakdown.py PID` (Rss/Pss/private per mapping).
- The window was opened without focus (`focus: false`, no `cx.activate`). Runs where the owner's pointer
  crossed the window show a few renders from the title-button hover styles; those runs were repeated.

### 13.2 Binary size and dynamic libraries
| Build | Stripped size |
|---|---|
| `opt-level=3`, fat LTO, wayland + x11 | **19.29 MB** (19 291 096 B) |
| `opt-level=3`, thin LTO | 20.97 MB |
| `opt-level="s"`, fat LTO | **14.14 MB** |
| `opt-level=3`, fat LTO, wayland only | 18.35 MB |
| `opt-level=3`, fat LTO, x11 only | 18.21 MB |
- `ldd` (wayland + x11): `libxcb.so.1 libxkbcommon.so.0 libxkbcommon-x11.so.0 libgcc_s.so.1 libm.so.6 libc.so.6 libXau.so.6 libXdmcp.so.6 libxcb-xkb.so.1 ld-linux-x86-64.so.2`. Wayland only: `libxkbcommon.so.0 libgcc_s.so.1 libm.so.6 libc.so.6`.
- Loaded at run time (`/proc/<pid>/maps`): `libwayland-client` (wayland-backend `dlopen` feature, `crates/gpui_linux/Cargo.toml:95-99`), `libvulkan.so.1` + the NVIDIA ICD (`libGLX_nvidia`, `libnvidia-glcore`, `-gpucomp`, `-rtcore`, `-glvkspirv`, `-allocator`, `-glsi`, `-tls`, which pull `libX11`/`libxcb-*`/`libdrm`), `libdbus-1`, `libsystemd`, and `libEGL` (glvnd). In the default config glvnd also loads **every EGL vendor** for wgpu's GL probe: Mesa (`libEGL_mesa` → `libgallium` → `libLLVM`, `libicuuc`, `libxml2`, `libstdc++`, `libgbm`, `libdrm_amdgpu/intel`, `libsensors`…) and NVIDIA (`libEGL_nvidia`, `libnvidia-eglcore`, `libnvidia-egl-wayland*`/`-gbm`/`-xcb`/`-xlib`) — roughly 60 shared libraries mapped vs 38 with EGL hidden. FreeType and fontconfig are never loaded.
- Compared with the 19.3 MB default: `opt-level="s"` −5.2 MB with no measurable startup change; dropping x11 −0.9 MB; thin LTO +1.7 MB. The remaining bulk is wgpu + naga (Vulkan *and* GL backends + shader translation), cosmic-text/swash/skrifa, icu_segmenter data, image codecs (exr, tiff, dds, hdr, … §1.3), resvg, zbus/ashpd/oo7/notify-rust, accesskit.

### 13.3 Startup (spawn → …), median of 5 warm runs, Wayland unless noted
| Config | `main` | first render | first frame done |
|---|---|---|---|
| default | 1.3 ms | 149 ms | **186 ms** (181–193) |
| EGL vendors hidden (Vulkan only) | 1.3 ms | 106 ms | **141 ms** (133–154) |
| `opt-level="s"`, EGL hidden | 1.3 ms | 105 ms | 144 ms |
| X11 (XWayland), EGL hidden | 1.4 ms | 134 ms | 142 ms |
- `app_run` (platform up, inside `Application::run`) is reached at ~12 ms; ~95–140 ms then go to GPU adapter
  enumeration and device creation, which happens on the first `open_window`. Initialising the GL backend
  next to Vulkan costs ~45 ms of that.
- The **first run of a new binary** is ~2.5× slower (504 ms to first frame for the wayland-only build) and
  briefly uses ~40 MB more RSS: the NVIDIA driver's shader cache is per executable. Installers should
  expect the first launch after an update to be slow on NVIDIA. `__GL_SHADER_DISK_CACHE=0` makes every
  launch slow (451 ms).

### 13.4 Idle: window open and unfocused, 20 s settle, 30 s window
| Config | Rss | Pss | Private (clean+dirty) | Anonymous | Threads | CPU / 30 s | Main-thread wakeups | Frames drawn | GPU mem |
|---|---|---|---|---|---|---|---|---|---|
| **A** Wayland, default | 162.0 MB | 84.5 MB | 75.2 MB | 35.5 MB | 33 | 0–10 ms | **0** | **0** | 60 MiB |
| **B** Wayland, `__EGL_VENDOR_LIBRARY_FILENAMES=…/10_nvidia.json` | 128.6 MB | 77.9 MB | 71.0 MB | 31.2 MB | 33 | 0 ms | 0 | 0 | 60 MiB |
| **E** Wayland, EGL hidden (`__EGL_VENDOR_LIBRARY_FILENAMES=/nonexistent.json`) | **110.1 MB** | **68.0 MB** | **62.1 MB** | 25.5 MB | 33 | 0 ms | 0 | 0 | 56 MiB |
| E + `opt-level="s"` | 106.9 MB | 64.7 MB | 58.8 MB | 25.5 MB | 33 | 0 ms | 0 | 0 | 56 MiB |
| E + wayland-only build | 109.7 MB | 67.2 MB | 61.6 MB | 25.5 MB | 33 | 0 ms | 0 | 0 | 56 MiB |
| **D** X11 (XWayland), default | 165.8 MB | 88.4 MB | 79.1 MB | 35.4 MB | 34 | **110 ms** | **280 /s** | 0 | 61 MiB |
| X11, EGL hidden (20 s window) | 112.9 MB | 71.0 MB | 65.2 MB | 24.5 MB | 34 | 90 ms / 20 s | 280.6 /s | 0 | 57 MiB |
- **Frames at idle: none** on either backend (render counter unchanged across the window). X11's 280
  wakeups/s are the refresh-rate timer of §3.2 (monitor at 279.87 Hz), costing ~0.37 % of one core.
- **Per-thread voluntary context switches/s over 30 s (A):** main 0; `Worker-0..19` 0; `Timer` 0; `smol-1` 0;
  `async-io` 0; `zbus::Connectio` ×3 0; `[vkps] Update` (NVIDIA driver) 4.0; one unnamed driver thread
  (created during Vulkan init) 0.5; two more unnamed driver threads 0; `[vkcf]`/`[vkrt] Analysis` 0.
  X11 adds a `Clipboard` thread (0/s). With CPU affinity limited to 4 cores (`taskset -c 0-3`) the process
  has 17 threads (4 workers) instead of 33 — the pool follows `available_parallelism()` (§3.7).
- **Where the RSS goes** (`smaps_breakdown.py`, default config A → E):
  | Mapping | A: Rss / Pss / private | E: Rss / Pss / private |
  |---|---|---|
  | `libnvidia-gpucomp.so` (shader compiler) | 30.4 / 5.5 / 3.7 | 30.4 / 5.7 / 3.7 |
  | `libLLVM.so.23` (via Mesa EGL) | 25.8 / 2.9 / 1.3 | — |
  | `[heap]` (app + GPUI + driver mallocs) | 18.9 / 18.9 / 18.9 | 15.8 / 15.8 / 15.8 |
  | the `hello` binary (file-backed, clean) | 14.0 / 14.0 / 14.0 | 14.4 / 14.4 / 14.4 |
  | `libnvidia-glcore.so` | 13.4 / 4.8 / 3.0 | 13.4 / 5.4 / 3.1 |
  | `/dev/nvidiactl` mappings | 12.9 / 12.9 / 12.9 | 9.6 / 9.6 / 9.6 |
  | `libnvidia-rtcore.so` | 10.9 / 10.9 / 10.9 | 10.8 / 10.8 / 10.8 |
  | `libnvidia-eglcore.so` | 9.7 / 2.5 / 1.7 | — |
  | `[anon]` | 4.6 / 4.6 / 4.6 | 2.9 / 2.9 / 2.9 |
  | `libnvidia-glvkspirv.so` | 3.4 / 1.0 / 0.2 | 3.4 / 1.3 / 0.2 |
  | `libgallium` (Mesa) | 3.1 / 1.7 / 1.6 | — |
  | font files (mmapped) | 1.3 / 0.7 / 0.7 | 1.3 / 0.7 / 0.7 |
  | everything else | ~13 / ~3 / ~1 | ~10 / ~1 / ~1 |
  | **total** | **161.9 / 82.6 / 74.9** | **110.4 / 68.1 / 62.2** |
  So with GL out of the way, ~70 MB of RSS is the NVIDIA Vulkan driver, ~19 MB is our heap + anon, ~14 MB is
  our binary's text. The heap of a 400-row chat view with its layout and glyph caches is under 20 MB.
- **System font scan** (what `CosmicTextSystem::new` does at startup, §5.2), measured with a standalone
  `cosmic_text::FontSystem::new()` (`gpui-client/vendor/scratch/fontscan/`): 1 095 faces indexed in 9–10 ms,
  +0.95–1.0 MB RSS, page cache warm. Not worth patching for memory; patch only if we want deterministic fonts.

### 13.5 Levers tried
| Lever | Effect here |
|---|---|
| Hide all EGL vendors (`__EGL_VENDOR_LIBRARY_FILENAMES=/nonexistent.json`) → wgpu's GL backend finds nothing, Vulkan unaffected | **−52 MB RSS, −16.5 MB Pss, −13 MB private, −45 ms startup, −4 MiB GPU memory.** Do it properly with a patch: enumerate GL only if no Vulkan adapter configures the surface (`crates/gpui_wgpu/src/wgpu_context.rs:486-494,531`). Env-var form must not ship (a machine without Vulkan would lose its fallback). |
| Only NVIDIA's EGL vendor | −33 MB RSS (avoids Mesa + LLVM only) |
| Force the GL backend (`VK_LOADER_DRIVERS_SELECT=nonexistent`) | App fails to start: NVIDIA GL adapter has "no compatible surface formats" on Wayland and XWayland |
| `WGPU_BACKEND`, `WGPU_POWER_PREF` | Not read by GPUI (descriptor built by hand, §3.6); `ZED_DEVICE_ID` is the only selector |
| Present mode | Hard-coded: Mailbox (Wayland), Fifo (X11); no env/option. Not an idle factor (nothing is presented at idle) |
| `opt-level="s"` | −5.2 MB binary, −3 MB RSS, startup unchanged |
| Wayland-only build | −0.9 MB binary, no RSS change |
| CPU affinity 4 cores | 33 → 17 threads, RSS unchanged |
| `__GL_SHADER_DISK_CACHE=0` | Worse: +25 MB RSS, 451 ms startup; `[vkps]` still wakes 4/s |
| Atlas / MSAA / swapchain count | No knobs: atlases start at 1024² per kind and grow by adding textures; no MSAA anywhere; latency 2 (§3.6). GPU memory 56–60 MiB for a 1280×800 window. |
| System fonts off | Requires patching `crates/gpui_linux/src/linux/platform.rs:132`; worth ~1 MB / ~10 ms |
