# The native client on Windows: what the owner checks

Everything Windows-specific in `gpui-client/` was written on Linux. It is type-checked there for the Windows
target (`tools/win-check.sh`), compiled by the Windows Native Check, and partly *run* on that job's runner, which
has no GPU (`ci/win-gpui-check-log`: `selftests.log`, `unit-tests.log`, `shaders.log`, `idle.log`). Nothing has run
on a real Windows desktop or a real GPU. This is the list of what only a Windows machine can tell, in order;
each step says what to run, what you should see, what a failure looks like and what to send back.

Send back, for anything that fails: the command, its whole output (`DECIBELL_LOG=info` set), and for a crash the
last lines before it. PowerShell throughout; `$repo` is the checkout.

## 0. Build

Once (HANDOFF §2 has the reasons): Rust stable (MSVC), Git for Windows, Visual Studio Build Tools with the
Windows SDK (`fxc.exe` for release builds), protoc on `PATH`, and the client's own vcpkg tree:

```powershell
git clone https://github.com/microsoft/vcpkg C:\dev\vcpkg-gpui
git -C C:\dev\vcpkg-gpui checkout e5a1490e1409d175932ef6014519e9ae149ddb7c
C:\dev\vcpkg-gpui\bootstrap-vcpkg.bat -disableMetrics
C:\dev\vcpkg-gpui\vcpkg.exe install "ffmpeg[nvcodec,amf,qsv,dav1d,svt-av1]:x64-windows" "--overlay-ports=$repo\gpui-client\tools\vcpkg-overlay"
```

Every shell:

```powershell
$env:VCPKG_ROOT = "C:\dev\vcpkg-gpui"
$env:Path = "C:\dev\vcpkg-gpui\installed\x64-windows\bin;$env:Path"
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"
$env:DECIBELL_LOG = "info"
cd $repo\gpui-client
bash tools/fetch-gpui.sh                    # Git Bash; once, and after a patch or the pin changes
cargo build --release -p decibell-app       # target\release\decibell.exe
```

- Expect: a build without errors. A debug build (`cargo build -p decibell-app`) compiles GPUI's shaders when the
  window opens instead of at build time, and needs no `fxc.exe`.
- Failure to send: the first `error` block. `Failed to find fxc.exe`: install the Windows SDK or set
  `GPUI_FXC_PATH`. A `Shader compilation failed for surface_…` is ours (GPUI patch `0020`): send it whole.
- **The release binary opens a console window beside the app** (no `windows_subsystem` yet): that is where the
  log lines below appear. Known, listed in HANDOFF.

## 1. Self-tests that need nothing

```powershell
.\target\release\decibell.exe --selftest boot,idle-frames,themes,window-frame
.\target\release\decibell.exe --selftest video-probe
.\target\release\decibell.exe --selftest kit-idle,kit-switch,kit-select,kit-modal,kit-confirm,kit-scroll,kit-toast
.\target\release\decibell.exe --selftest input-typing,input-undo,input-ime,input-atoms,input-bytes-cap,input-select-filter,input-caret-idle,input-clipboard,input-password
.\target\release\decibell.exe --selftest chat-history,chat-switch,chat-resize,chat-selection,chat-rows,chat-idle,chat-hover
.\target\release\decibell.exe --selftest render-cost,encoder-software
```

Each prints `selftest <name>: PASS|FAIL …` and exits 0 / 1. Keep the pointer off the test window (a hover is a
frame; the idle tests retry twice).

- `video-probe` is the one to read: it prints, per codec, which decoder the probe settled on, e.g.
  `H.264: D3D11VA (decoded the test frame)`, then decodes and draws the compiled-in frame with that decoder and in
  software. **Expected on NVIDIA, AMD and Intel GPUs of the last years: `D3D11VA` for all three codecs** (AV1 needs
  an RTX 30 / RX 6000 / 11th-gen Intel or newer; older GPUs show `Software (dav1d)` for AV1 with the driver's
  reason on a line `AV1 with d3d11va: not used, …`). A failure prints which step did not happen; `Software` where
  the GPU certainly has a decoder is a failure too: send the `not used` lines.
- Known to fail on Windows today, not yours to chase: `chat-scroll-spec` (assumes Linux's wheel step), `regional`
  (drives glibc locales), `content-scroll-fixture` / `content-rows` (start a D-Bus daemon).

## 2. The local stack from Windows

The self-tests that sign in, and the app itself with `--login`, need the local test stack, which runs on Linux
(`tools/dev/local-stack.sh up`, bash + the two servers). Two ways:

- **WSL2 on the same machine** (simplest): run the stack inside WSL; Windows reaches a WSL2 listener on
  `127.0.0.1`. Build the client with `--features dev-local`, set `$env:DECIBELL_CENTRAL = "127.0.0.1:18080"`,
  `$env:DECIBELL_DEV_PASSWORD = "decibell-test-1"`, `$env:DECIBELL_DEV_DIR` to a folder holding a copy of the
  stack's `run\accounts.json` (the tests read `<dev dir>\run\accounts.json`), and a throwaway
  `$env:DECIBELL_USER_DATA_DIR`.
- **The Linux box over the LAN**: `--login` and the stack tests refuse a central that is not loopback, by design.
  Forward the ports instead (`ssh -L 18080:127.0.0.1:18080 -L <community ports> linux-box`); UDP media is not
  forwarded by ssh, so voice and streams need WSL2 or the real servers.

Never point a `dev-local` build at the live central, and never use your own account for tests.

## 3. Watching a stream (the Direct3D path)

In a voice channel of the local stack with a stream running (`tools/perf/streambot.sh` on Linux, or a second
client), watch it. With `DECIBELL_LOG=info` the console shows, once per decoder opened:

```
video: D3D11VA on the renderer's device
video: stream <uid>_screen: d3d11va decoder for H.265 opened in 12.3 ms
```

| Check | How | Expect | Failure looks like |
|---|---|---|---|
| Hardware decode, each vendor | watch a stream of each codec on the NVIDIA machine, an AMD one, an Intel one (integrated counts) | a picture, smooth; Settings → Codecs names `D3D11VA` per codec; the stream's stats overlay says `hardware`; Task Manager → GPU shows "Video Decode" busy | no picture and an error line; or `Software` named with a reason under the codec list; green / purple picture or swapped colours (NV12 planes); a picture shifted or with a garbage strip at an edge (coded size against picture size) |
| Software forced | `$env:DECIBELL_VIDEO_DECODER = "software"`, restart, watch | the same picture; Codecs says `Software (FFmpeg)` / `Software (dav1d)`; CPU clearly higher | no picture; wrong colours here but right with D3D11VA (or the reverse) points at one path's plane order |
| D3D11VA forced | `$env:DECIBELL_VIDEO_DECODER = "d3d11va"` | as the first row; a codec the GPU cannot decode shows **no picture and an error**, never silent software | |
| Two streams | watch two at once; open the mini-player for one | both play; closing one leaves the other untouched | one freezes while the other plays; a crash on closing |
| Resize, fullscreen, minimise | drag the window edge while a stream plays; maximise; minimise and restore; move the window to another monitor (other DPI) | the picture follows, no black frames beyond the resize itself | a stream that stays black after restore; a crash in `ResizeBuffers` |
| Sleep / driver reset | `Win+Ctrl+Shift+B` (resets the graphics driver) while a stream plays | the window goes blank briefly, redraws, and the stream comes back within a second or two after the next keyframe | the window stays blank; the stream never returns; a crash (send the log: device recovery is GPUI's own path plus ours) |
| Rounded corners, overlays | hover a stream tile (name plate, controls over the picture), open a menu over a stream | drawn over the picture, corners rounded as on Linux | picture drawn over the controls; square corners |
| The test streams (optional) | copy `gpui-client\vendor\streams` from the Linux box, then `.\target\release\decibell.exe --selftest video-hw,video-sw,video-two,video-idle,video-switch,video-order,video-recover,video-starve,video-stall` | PASS each; `video-hw` prints the decoder per codec | the failing test's lines |

What cannot be checked by a test there: the window's pixels are not read back on Windows, so **colours are yours
to judge** (compare a stream side by side with the Electron client: skin tones, a saturated red, a grey ramp).

## 4. At rest

The targets are the Linux ones: 0 frames while nothing changes, ≤ 30 wakeups a second of our own threads.

```powershell
$p = Start-Process .\target\release\decibell.exe -ArgumentList '--perf' -RedirectStandardError perf.log -PassThru
# sign in, open the view to measure, leave the pointer outside the window, click another window (unfocused), then
..\tools\perf\windows\row.ps1 -ProcessId $p.Id -Client gpui -Label "idle, #general, unfocused" -PerfLog perf.log
```

- Expect in the row: `frames drawn / presented` **0 / 0**; `wakeups_s` in the low tens at most. In
  `<run>-threads.txt`: the unnamed first thread (main) and `VSyncProvider` near 0 /s. `VSyncProvider` at your
  display's refresh rate means GPUI patch `0021` is not holding (something asks for a frame every blank: send the
  table and say what was on screen); a thread of `nvwgf2umx.dll` (NVIDIA's driver) around 80 /s is the driver's
  own and not counted against the target.
- Touchpad check for the same patch: two-finger scroll a long channel on a precision touchpad, lift, let it
  coast. Expect smooth scrolling and inertia. A scroll that freezes while the fingers rest and move again, or
  inertia that stops dead, is the patch (the vsync thread parked during a gesture): say so.
- The Electron client on the same machine, for the table: start it yourself, find its main process
  (`Get-CimInstance Win32_Process -Filter "Name='Decibell.exe'" | Where-Object CommandLine -notmatch '--type='`),
  put it in the same state, then `row.ps1 -ProcessId <that pid> -Client electron -Label "<the same label>"`.
- Startup: `..\tools\perf\startup.ps1 -Exe <path> -Out …` for each client (launch → visible window).
  Install size: `..\tools\perf\windows\install-size.ps1 <folder or files>`.

The table to fill (the Linux reference is HANDOFF §4; targets are ratios against Electron **on the same machine**):

| Target | Ratio | Scenario |
|---|---|---|
| Idle private memory (private working set) | ≤ 1/3 of Electron | `#general` open, visible, unfocused; also minimised and Home |
| Idle CPU | ≤ 1/10 | same |
| Idle wakeups, own threads | ≤ 30 / s | same; and focused |
| Frames at rest | 0 | same |
| Startup to a visible window | ≤ 1/3 | warm, saved sign-in |
| Install size | ≤ 1/4 | the exe + the FFmpeg DLLs it loads, against Electron's install folder |
| Watching 2 streams | CPU ≤ 1/3, GPU ≤ Electron | 2 × 1080p60 or the bot's streams |

## 5. The window

| Check | Expect | Failure |
|---|---|---|
| Title bar buttons | hover highlights each; minimise, maximise, **restore** (click maximise again), close all work | a button that does nothing: the caption buttons are the system's on Windows (the client only draws them), so say which |
| Windows 11 snap layouts | hovering the maximise button shows the snap flyout; `Win+Z` works | no flyout |
| Dragging, double-click, right-click | drag by the bar; double-click maximises and again restores; right-click on the bar opens the system menu | a double-click that maximises but never restores |
| Resize | every edge and corner resizes; the window stops at 800 × 600 | a top edge that does not resize |
| DPI | move between monitors of different scale; change the scale in Settings while it runs | text stays sharp, layout rescales; send a screenshot of anything blurry or clipped |

## 6. Later sections

Go Live (the screen / window grid, a game in exclusive fullscreen, stream audio per app), auto-away, hotkeys,
the clipboard, the keyring entry, regional formats, fonts and IME get their steps here as each lands on the
branch; HANDOFF "Windows" lists what is written and what is not.
