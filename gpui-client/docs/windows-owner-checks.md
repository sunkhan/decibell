# The native client on Windows: what the owner checks

Everything Windows-specific in `gpui-client/` was written on Linux. It is type-checked there for the Windows
target (`tools/win-check.sh`), compiled by the Windows Native Check, and partly *run* on that job's runner, which
has no GPU (`ci/win-gpui-check-log`: `selftests.log`, `unit-tests.log`, `shaders.log`, `idle.log`). Nothing has run
on a real Windows desktop or a real GPU. This is the list of what only a Windows machine can tell, in order;
each step says what to run, what you should see, what a failure looks like and what to send back.

Send back, for anything that fails: the command, its whole output (`DECIBELL_LOG=info` set), and for a crash the
last lines before it. PowerShell throughout; `$repo` is the checkout.

## What the CI runner already checks (don't repeat by hand)

Run 45983095122 (a GitHub Windows runner: no GPU, Direct3D 11 on WARP, a debug build) passed, and every push
runs again: the build and fxc on the shaders; `boot` (the window opens on a Direct3D device with the surface
pipeline, every bundled font face resolves under DirectWrite, the wide "Plus" / "Max" families included);
`idle-frames`, `themes`, `window-frame`; all seven `kit-*`; `input-undo`, `input-bytes-cap`,
`input-select-filter`, `input-caret-idle`; `chat-resize`, `chat-selection`, `chat-rows`, `chat-idle`,
`chat-hover`; `encoder-software`; **`video-probe`** (software decode of all three codecs, upload into Direct3D
textures, a row read back from the GPU, the surface drawn in a window, D3D11VA given up cleanly where there is
no video device); 295 unit tests; and `row.ps1` against the client at rest: 0 context switches / s, 0 frames,
`VSyncProvider` asleep (patch `0021`).

So on your machine the self-tests of §1 are a smoke test (one command, expect all PASS); what needs **you** is
what a runner cannot have: a real GPU and its decoders (§3), real monitors, windows and games (§6), a touchpad
(§4), the real desktop (§5, §7), your eyes for colour and fonts, and the numbers against Electron (§4, §9).

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
- A build from the checkout opens a console window beside the app: that is where the log lines below appear
  (the shipped build's executable identity, without a console, is the packaging work's).

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
- Made portable in round 2 and unrun on a desktop since: `chat-scroll-spec` (takes Windows' wheel step),
  `content-scroll-fixture` / `content-rows` (no D-Bus daemon off Linux). `regional` skips on Windows by design
  (it drives glibc's locale environment): §8 has the Windows check.

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

## 6. Go Live

The dialog has its own grid on Windows ("What to share": Screens / Windows), made by the client
(`crates/decibell-voice/src/sources/windows.rs`; the core only opens the source it is handed). In a voice
channel, press Share screen.

| Check | Expect | Failure looks like |
|---|---|---|
| The Screens tab | one tile per monitor, named "Entire screen" (one monitor) or "Screen 1", "Screen 2", each with a still of that monitor in a 16:9 frame; the stills refresh every 5 s (move a window and watch) | no tiles and a red line of text (send it); a still of the wrong monitor; a black still |
| The Windows tab | every ordinary open window with its title and app icon; **not** listed: minimised windows, Decibell itself, tool windows, windows on another virtual desktop. Open a new app: it appears within 5 s | a window missing that the Electron client lists (say which app); a blank or black still (some apps refuse to draw themselves: say which); a wrong or missing icon; the dialog freezing for a moment every 5 s (a hung app in the list) |
| Picking | a click selects a tile (accent border, tick); the footer's Go Live starts the stream; viewers see that screen / window | "Pick a screen or window to share first." although a tile is selected; the stream showing another monitor than the tile (the monitor numbering: send the tile's name and `DECIBELL_LOG=info` output, the line `[capture] screen:N …`) |
| A second monitor at another scale / to the left of the primary | its still and its stream are the right monitor, not offset | a still cut off or showing part of the neighbour |
| A window stream | resize and move the shared window: the stream follows; minimise it: the stream freezes or ends with a message, no crash | |
| A game | windowed and borderless: pick its window, as any other. **Exclusive fullscreen**: pick the *screen* it is on (a fullscreen game's window gives a black or frozen picture with every capture API); alt-tab in and out while streaming: the stream recovers within a second or two | a stream that stays black after alt-tabbing back (the core's DXGI re-acquire: send the log) |
| The dialog closed | Task Manager → Details → decibell.exe: thread count back to what it was before the dialog opened within a few seconds; `row.ps1` at rest afterwards still 0 frames | wakeups every 5 s with the dialog closed (the listing task outliving the dialog) |
| Stream sound per app | Options → Share sound: "All apps", "Only these", "All except" list the apps that have a sound session, the picked window's app marked "this window"; viewers hear what the choice says and never Decibell itself | an empty list while something plays; viewers hearing an unticked app |

## 7. The desktop services (from the platform work)

- **Single instance**: start Decibell, minimise it, start it again from the Start menu: no second window, the
  first comes back to the front within a second. Start it again from a command prompt while another program
  is in front: it comes forward, or at least its taskbar button flashes. Two profiles
  (`set DECIBELL_USER_DATA_DIR=…` in one prompt): two windows.
- **`decibell://`** (after the installer registers it): with Decibell running, open
  `decibell://invite/<a real code>` from Win+R or a browser: the window comes forward with the join dialog; with
  Decibell closed it starts and shows the dialog after sign-in. The installer's keys, under
  `HKCU\Software\Classes\decibell`: `(Default)` = `URL:Decibell`, `URL Protocol` = `` (empty string),
  `DefaultIcon\(Default)` = `"<install dir>\decibell.exe",0`, `shell\open\command\(Default)` =
  `"<install dir>\decibell.exe" "%1"`. The app never writes the registry.
- **Attention**: have someone call you while Decibell is behind another window: its taskbar button flashes
  until you bring it to the front; not when your status is Do Not Disturb.
- **Taskbar progress**: download a large attachment: the taskbar button shows the progress, yellow when you
  pause it, and clears when it ends.
- **Quit guard**: close the window while a file uploads: "A file is still uploading" asks; Keep uploading /
  Enter / Escape keep the window, Quit anyway closes it.
- **Window state**: move the window to a second monitor (ideally at another scale), resize it, close, start:
  same monitor, place and size; maximise, close, start: maximised, and Restore gives the old size. Disconnect
  that monitor, start: the window opens on the remaining one.
- **Screen lock and auto-away**: Win+L, wait, unlock: your status showed Away while locked (another account
  sees it). Set Settings → auto-away to 1 minute and leave the mouse and keyboard alone: Away after a minute;
  move the mouse: Online again within about a second.
- **Copy image**: right-click a picture → Copy image, paste into Paint, Word and a browser.
- **Keyring**: Credential Manager → Windows Credentials shows a generic credential named
  `Decibell:url=decibell://e2ee-local-key/…`, nothing named `zed:`.

## 8. The rest of the table

| Feature | Do | Expect |
|---|---|---|
| Fonts | look at the server tiles and the "Decibell" wordmark beside the Electron client | the same wide letterforms (the families "Decibell Sans Plus" / "Decibell Sans Max"); `--selftest boot` passes ("every bundled face resolves"). A tile label in a narrower or a system font is the bug the CI runner showed: send `boot`'s output |
| Emoji | a message with 😀 👍🏽 🇹🇷 | Twemoji pictures, as in Electron (never Segoe UI Emoji's) |
| Text in other scripts | paste Japanese, Arabic, a rare symbol | drawn by DirectWrite's own fallback; boxes (tofu) for a script Windows has a font for is a failure |
| IME | type with the Japanese or Chinese IME in the message box | the candidate window sits at the caret; composition text is underlined; Enter commits without sending |
| Regional formats | `--screen main-server --selftest regional-read` prints what the client read from Windows' regional settings; compare message dates and times with the Region settings' short date and time | day / month order, separator, 12 / 24 h and digit grouping as Windows has them, English month names |
| Hotkeys | Settings → Keybinds: record Ctrl+Shift+M for mute, a bare key for push-to-talk, a mouse side button | the summary names the Windows backend; mute works with another app focused and the combo no longer reaches that app; push-to-talk works while a game has focus and the key still reaches the game; on a non-US layout the recorded key is the one pressed |
| Key labels (GPUI patch 0040, `ToUnicodeEx`) | add German (Germany) as a second input language; with it active, record the key right of T for mute; switch to English with Win+Space while Settings → Keybinds is open | under German the binding reads "Ctrl + Shift + Z" (the US "Y" key), the ´ key next to Backspace reads "´", ß reads "SS"; after Win+Space the same binding reads "Y" without reopening the tab. "Y" under German is the failure: send `decibell --layout-labels` from a development build (one line of labels) |
| File type filters (GPUI patch 0041) | Settings → Account → Change picture; Server Settings → Overview → the server picture | the dialog's type list shows "Images" first (only pictures listed), "All files" second |
| Restart to update (installer build) | with an update downloaded and a file uploading, press Restart in About | "A file is still uploading" asks; Keep uploading keeps Decibell open; Quit anyway restarts into the new version with no old invite link reopening |
| Crash report contexts | with a test DSN (the owner's Sentry), crash a development build (`DECIBELL_TEST_PANIC=x`) | the event has `contexts.device.memory_size` (Windows' `GlobalMemoryStatusEx`), `contexts.app`, the tag `connected_servers`; no `contexts.gpu` on Windows (the renderer is Direct3D: not read yet) |
| Links, files | click a link; Open and Show in folder on a finished download; attach through the file dialog; drop files on the window | the default browser; Explorer with the file selected; the upload starts. Open on a downloaded program (`.exe`, `.bat`, `.msi`, …) asks first ("… can run programs on your computer"), then runs it as Explorer would: the same as Electron |
| Clipboard | copy and paste text both ways with Notepad; paste a screenshot (Win+Shift+S) into the message box | text keeps its line breaks; the screenshot becomes an upload |
| Devices | Settings → Audio lists inputs and outputs by their Windows names; unplug the chosen headset in a voice channel | the lists follow; voice moves to the default device |
| DPI | 100 %, 150 %, 175 % and a per-monitor mix | sharp text at each; a stream tile's picture not blurred beyond its size |
| Calls | a DM call in and out | ring (not under Do Not Disturb), the call stage, the taskbar flash above |

## 9. Measuring the Electron client on the same machine

The table in §4 is a ratio, so both clients are measured on this machine, with the same script, in the same
states. For the Electron client:

1. Start the installed client yourself, signed in as usual (no script starts it: it would open your profile
   unasked). Close the native client first, or make sure you pass process ids: both are `decibell.exe` to
   Windows.
2. Its main process: `Get-CimInstance Win32_Process -Filter "Name='Decibell.exe'" | Where-Object CommandLine -notmatch '--type=' | Select-Object ProcessId, CommandLine`. `row.ps1 -ProcessId` measures that process and all its
   children (renderer, GPU process, utilities), summed.
3. Put it in the state of the row (the same server and channel as the native client's row, visible, unfocused,
   the pointer outside the window), then
   `..\tools\perf\windows\row.ps1 -ProcessId <pid> -Client electron -Label "<the same label>"`.
   The `frames` cell stays empty for Electron (no counter to read); its idle cost shows in `cpu_core_pct`,
   `wakeups_s` and `gpu_3d_pct`.
4. Rows to take for each client, labels identical: `idle, #general, unfocused` · `idle, #general, minimised` ·
   `idle, Home, focused` · `voice, alone, muted` · `watching 1 stream 1080p60` · `watching 2 streams`.
5. Startup: `..\tools\perf\startup.ps1 -Exe "$env:LOCALAPPDATA\Programs\decibell-electron-client\Decibell.exe" -Out ..\tools\perf\results\electron-startup`
   and the same with the native exe (keep `-Runs` small: each run signs in). Install size:
   `install-size.ps1` on Electron's install folder against `decibell.exe` plus the DLLs beside it.
6. Send `tools\perf\results\windows-table.md` and the `*-threads.txt` files.

## 10. The signed-in self-tests on Windows, by PARITY section

Most of the client is the same code on both systems over the same core, and its self-tests sign in against
the local stack. None of them has run on Windows (the CI runner has no servers). One sitting covers them:

1. The stack in WSL2 (§2), a `dev-local` build, and the peer the two-client tests start:
   ```powershell
   cargo build --release -p decibell-app --features dev-local
   cargo build --release -p decibell-model --features local-central --bin decibell-headless
   $env:DECIBELL_CENTRAL = "127.0.0.1:18080"; $env:DECIBELL_DEV_PASSWORD = "decibell-test-1"
   $env:DECIBELL_DEV_DIR = "<folder holding run\accounts.json copied from the stack>"
   ```
2. Run each line below (one window at a time; keep the pointer off it), and send every line's summary
   (`selftest: N passed, M failed`) with the output of anything that failed. A test that says `SKIP` names
   what it lacked: some still lean on Linux helpers (a D-Bus daemon, KWin, `sha256sum`) and say so; that is
   a finding to send, not a failure of yours.

| § | Command (`.\target\release\decibell.exe --selftest …`) | By hand, because it needs Windows itself |
|---|---|---|
| 10.1 Accounts (PARITY §1) | `shell-signin,shell-navigate,shell-members,shell-idle,shell-signout,settings-devices-signout,settings-password,settings-rename` | Settings → Devices lists this install as "Windows" with the app version (never the PC's name). Sign in on a second Windows install: the first is told. A central whose certificate changed: the re-trust prompt |
| 10.2 People (§2) | `profile-popup,status-custom,friends-flow,friends-page,user-menu-moderation,settings-privacy-dms,picture-editor` | Change the profile picture through **the Windows file dialog** (a `.jpg` taken by a phone, held upright; a `.png`; cancel): the editor opens with the picture the right way up |
| 10.3 Direct messages and calls (§3) | `dm-open-history,dm-send-receive,dm-read-sync,dm-read-rules,dm-e2ee,home-idle,call-ring,join-dm-composer` | A call with another machine, then **share a screen and a window into the call from Windows** (the Go Live dialog of §6 opens from the call bar): the other side sees it; stop; the call goes on |
| 10.4 Servers (§4) | `servers-browse-join,servers-join-link,invite-manage-live,invite-friends-live,welcome-banner,servers-idle,server-settings-open,overview-save,members-search,audit-log-live,bans-live` | Change a server's picture through the file dialog |
| 10.5 Channels and roles (§5) | `channel-create-delete,channel-reorder,category-sync-drop,channel-settings-echo,category-sync,roles-edit-echo` | Drag a channel into a category with the mouse (the test drives the model) |
| 10.6 Chat (§6) | `chat-live,composer-type-send,composer-draft,composer-burst,composer-slowmode,composer-refused,composer-edit,composer-reply,composer-attach,composer-drop,composer-paste-image,emoji-picker,gif-picker,composer-type-to-focus,composer-idle,composer-typing-cost` then `content-images-live,content-scroll-images,content-cache-bound,content-gif,content-viewer,content-embed,content-invite,content-download,media-live,media-idle,media-video-play,media-audio-play,media-persist` then `search-live,search-supersede,search-refused,search-paging,search-jump,search-narrow,search-encrypted,search-nothing-stored,header-members-toggle,search-idle,search-typing-cost` | A message with a code block and with `$x^2$` (drawn as in Electron); play a video and an audio attachment (sound on the device §8 names; no click at pause or seek) |
| 10.7 Voice (§7) | `voice-join,voice-speaking-idle,voice-mute,voice-cues,voice-switch,settings-audio-meter` | With a second person: join, switch channel, leave; both hear each other through **WASAPI** with echo cancellation, noise suppression and gain as set in Settings → Audio (speak with speakers on: the other side hears no echo); the right-click volume of a person; mute, deafen, a moderator's server-mute; the lock in the voice bar (MLS) and the stats popover's numbers moving |
| 10.8 Streams (§8) | `voice-watch,voice-stall,voice-golive-dialog` | While you stream from Windows: the stream's **thumbnail** on your tile in the channel list for people not watching (refreshed every 15 s); move to another voice channel and choose to take the stream along; **stream sound** heard by a viewer, and sealed (the viewer's stats show an encrypted stream) |
| 10.9 Settings (§9) | `settings-open,settings-appearance-apply,settings-e2ee,settings-keybinds-record,settings-network-caps,settings-unsaved-shake,settings-idle` | Account: the avatar and name as on Linux. Appearance: every theme. Network: the download folder shown is your real Downloads folder (**also when you moved it** in Explorer's properties: the client reads `%USERPROFILE%\Downloads`, so a moved folder is a finding). About, the updater and the crash reporter are the installer's checks (`PACKAGING.md` §11) |
