# Transfers panel — downloads & uploads manager in the title bar (2026-10-04)

A browser-style transfers popover (Chrome's / Firefox's downloads bubble) opened from a button in
the custom title bar. It lists attachment downloads and uploads with live progress, speed and ETA,
and manages them: pause, resume, cancel, retry, open, show in folder, clear.

## Owner decisions

- **Save location**: downloads go straight to a folder, no dialog. The default is the OS Downloads
  folder, and Decibell has its own adjustable download folder in Settings. Explicit "Save as…"
  (the image/video context menu) still opens the dialog. Both kinds show up in the panel.
- **History**: finished downloads persist across restarts (last 100). An unfinished download is
  paused on quit and can be resumed next launch. Uploads are per session, because an upload only
  exists while its message is sending.
- **Layout**: two tabs, **Downloads | Uploads** (segmented control), not one mixed list.
- **Button**: always visible. An empty tab says so.

## What exists today

- Four save sites, each with its own copy of the same flow: the file card and the audio player
  (`AttachmentList.tsx`), the video player (`PersistentVideoLayer.tsx`), and "Save as…"
  (`ImageContextMenu.tsx`). Each one opens the save dialog, has main buffer the **whole file**
  (`netFetch` → `ArrayBuffer`), sends it over IPC to the renderer, and sends it back over IPC to
  `fs.writeFile`. There's no progress, pause, resume or cancel, and the only feedback is a toast at
  the end.
- Uploads have live state in `attachmentsStore` (`uploading` + `transferredBytes`), but a pending
  is removed the moment its message sends, so nothing remembers it afterwards. DMs have no
  attachments, so uploads only come from channels.
- The speed caps from earlier today (`downloadPacer.ts`, `uploadPacing.ts`) already apply per file.
  The manager reuses them.

## UX

### Title-bar button

A `TitleButton` (`w-11`, full bar height) just left of Minimize, with a 1 px divider before the
window controls. Tooltip and aria-label: "Transfers" (+ "· 2 active").

| State | Look |
|---|---|
| Idle | Download glyph (arrow into a tray), `text-secondary` |
| Active | A 2 px ring around the glyph, filled to the combined progress of every active transfer, in the accent colour |
| Only paused | Same ring in `text-muted` |
| Something finished while the panel was closed | Small accent dot, top-right; cleared when the panel opens |
| Something failed while closed | The dot uses `error` |
| A download just started | The glyph's arrow drops once (`@keyframes`); nothing under `prefers-reduced-motion` |

`Ctrl/Cmd+J` toggles the panel (Chrome's downloads shortcut; nothing in the app binds it today).

### Panel

Uses the existing popover pattern (`StreamAudioPopover`): a portal, `fixed`, right-aligned under
the button, `rounded-lg border border-border bg-bg-secondary shadow-modal`, 360 px wide. The list
grows to about 6 rows, then scrolls. Closes on outside click or Esc; focus goes into the panel and
back to the button when it closes.

- **Header**: `SegmentedControl` **Downloads · 2 | Uploads · 1**. The count is in-progress items
  (active + paused) and is left off at 0. On open, the panel shows whichever tab had the newest
  activity since it was last opened, otherwise the tab you last used.
- **Footer**: on the left, "↓ Limited to 1 MB/s" (or "↑ …" on Uploads), shown only when a cap is
  set and linking to Settings → Network. On the right: **Open folder** (Downloads tab only) and
  **Clear** (removes finished, failed and cancelled rows; never touches files).
- **Empty**: "No downloads yet — files you download from chats show up here" / "No uploads this
  session".

### Download row

```
[icon]  holiday-video.mp4                      [⏸] [✕]
        ▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░
        18.4 of 44 MB · 1.0 MB/s · 26 s left
```

- **Icon**: the attachment's 320 px thumbnail for images and videos (via the cached
  `decibell-attachment://` thumb), otherwise a kind glyph (document / audio / archive).
- **Progress bar**: only while active or paused. A `bg-bg-darkest` track with an accent fill;
  paused uses `text-muted`.

| State | Status line | Actions | Row click |
|---|---|---|---|
| active | `18.4 of 44 MB · 1.0 MB/s · 26 s left` | Pause, Cancel | — |
| paused | `Paused · 18.4 of 44 MB` | Resume, Cancel | — |
| done | `44 MB · #design · Studio · 3 min ago` | Show in folder, Remove | Open the file |
| failed | `Failed — Not connected to Studio` | Retry, Remove | — |
| cancelled | `Cancelled` | Retry, Remove | — |
| deleted | `Deleted` (muted; the file is gone from disk) | Download again, Remove | — |

- **Right-click**: Open · Show in folder · Go to message · Remove from list.
- **Opening risky files**: for executables and scripts (`.exe .msi .bat .cmd .ps1 .vbs .scr .lnk
  .sh .app .dmg .pkg .jar`, …), Open goes through `ConfirmModal`: "This file can run programs on
  your computer. Only open it if you trust ‹sender›." Show in folder never asks. The file comes
  from another user, so this check ships together with Open.

### Upload row

```
[preview] screenshot.png                       [⏸] [✕]
          ▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░░
          3.1 of 4.8 MB · 1.2 MB/s · to #general
```

| State | Status line | Actions | Row click |
|---|---|---|---|
| uploading | `3.1 of 4.8 MB · 1.2 MB/s · to #general` | Pause, Cancel | — |
| paused | `Paused · 3.1 of 4.8 MB · to #general` (the message waits) | Resume, Cancel | — |
| sent | `Sent to #general · Studio · 4.8 MB` | Remove | Go to the channel (the message itself in phase 3) |
| failed / cancelled | `Failed — ‹reason›` or `Cancelled`, + `· message sent without it` when the message still went out | Remove | — |

- **Preview**: the existing `previewUrl` blob for images; a kind glyph for anything else.
- **What counts**: rows appear when the send starts. Files still sitting as composer chips aren't
  transfers yet. Cancel is the same abort the composer already uses, so the message still sends
  with its other attachments, exactly as today.

### Settings → Network → "Downloads"

- **Location**: the current path (shortened in the middle) + **Change…** (a folder dialog run by
  main) + **Reset** (back to the OS Downloads folder).
- **Ask where to save each file**: toggle, off by default. Easy to drop if not wanted.

## Architecture

### Main: `electron/main/downloads.ts` (the download manager)

Main owns every download from start to finish. Bytes stream straight to disk and never cross IPC.

```ts
interface DownloadRecord {
  id: string;                      // uuid
  serverId: string;
  attachmentId: number;
  filename: string;                // display name (sanitised sender name)
  path: string;                    // final path, reserved at start
  mime: string;
  kind: AttachmentKind;
  totalBytes: number;              // plaintext size from the attachment
  receivedBytes: number;
  state: "active" | "paused" | "done" | "failed" | "cancelled";
  error?: string;
  startedAt: number;
  finishedAt?: number;
  context: {
    serverName: string;
    channelId: string;
    channelName: string;
    messageId: number;
    sender: string;
  };
  sealed?: { wrappedKey: string; chunkBytes: number }; // encrypted channels; see below
}
```

- **Destination.** The folder is the configured one, or `app.getPath("downloads")`.
  - *Filename*: the name comes from another user, so it is sanitised. Strip path separators and
    control characters, drop trailing dots and spaces, rename Windows reserved names (`CON`,
    `NUL`, …), cap the length, and fall back to `attachment`.
  - *Collisions*: the final name is made unique (`name (1).ext`) and reserved by creating
    `‹final›.part`.
  - *Save as…*: main shows the dialog itself, so the renderer never hands main a destination path.
- **Transfer, plain attachments.**
  - `net.fetch` with `Range: bytes=‹partSize›-`, read through `pacedBody` (the speed cap), written
    to an `fs.WriteStream` that respects backpressure.
  - The server answers 206 for a range. A 200 on a resume means it ignored the range, so the
    manager truncates the part file and starts over.
  - Attachment ids are immutable, so resuming by Range is safe without ETags.
- **Transfer, encrypted attachments.**
  - The manager loops `fetchDecryptedAttachment` over 4 MiB plaintext windows starting at
    `receivedBytes`; that function already does range planning, decryption and the speed cap.
  - At start it takes a snapshot of the key info, so a server disconnect (which clears the key
    registry) doesn't break a download in progress.
  - To resume after a restart, the per-file key is stored wrapped with `safeStorage` (the
    `e2eeLocalKey` pattern) and deleted once the download finishes. A plaintext key on disk would
    be no worse than the plaintext part file, but wrapping it costs nothing.
- **Finish.** Check that the received size matches `totalBytes`, flush, rename `.part` to the final
  name (picking a new unique name if something appeared at that path meanwhile), then set `done`.
  Out of disk space or a write error ends as `failed` with the reason ("Disk full").
- **Pause and cancel.** Pause aborts the fetch and keeps the `.part` file; Resume and Retry continue
  from its size. Cancel aborts and deletes the `.part`.
- **Quit.** On `before-quit`, active downloads are paused and saved. Next launch they show as Paused
  with Resume, and are not resumed automatically. Resuming needs the server connected (main
  registers its target at community auth); otherwise the row fails with "Not connected to ‹server›".
- **Persistence.** `userData/downloads.json`, holding the last 100 records (oldest finished ones are
  dropped first). It's written atomically (write to a temp file, then rename), debounced, and only
  on state changes, never on progress ticks. When the panel opens, main stats the finished records
  to find files that were deleted.
- **Security.** The renderer refers to downloads by id only. `open`, `showInFolder` and `remove`
  act on the path main recorded (`shell.openPath`, `shell.showItemInFolder`), never on a path the
  renderer sends. Keys never cross back to the renderer.
- **IPC** (preload `window.decibell.downloads.*`):
  - `start(args) → { id } | null` (null when a Save-as dialog is dismissed)
  - `pause`, `resume`, `cancel`, `retry`, `remove`, `open`, `showInFolder` — each takes an `id`
  - `clearFinished()`, `openFolder()`, `list()`
  - `configure({ dir, askEachTime })`, `pickFolder()`
  - The event `downloads_changed { record, speedBps, etaS }` goes out at about 4 Hz per active
    download plus immediately on any state change, over the existing main→renderer event channel.
    Speed is an EWMA over about 3 s.
- **Taskbar / dock progress.** `mainWindow.setProgressBar(fraction, { mode })` over every active
  transfer. The renderer computes the fraction, since it knows both directions, and sends it at
  ≤ 4 Hz; `paused` mode when everything is paused.

### Renderer

- **`stores/transfersStore.ts`**
  - A mirror of the downloads, seeded by `list()` at startup and patched by `downloads_changed`.
  - The session's upload history, keyed by `pendingId`: `{ filename, totalBytes, transferredBytes,
    speedBps, state, serverId, channelId, nonce, finishedAt }`. Entries are recorded when the send
    starts (`startQueuedUpload`) and on every progress and terminal transition, so they outlive
    `removePending`. Upload speed is an EWMA in the store.
  - `lastOpenedAt` and the per-tab activity times that drive the attention dot and the default tab.
  - Selectors follow the stable-ref rule (`lib/empty.ts`, no `.filter` inside a selector).
- **`features/transfers/`**: `TransfersButton` (mounted in `Titlebar`), `TransfersPanel`,
  `DownloadRow`, `UploadRow`, `DownloadsSettings` (rendered in `NetworkTab`), and
  `startDownload(attachment, context, { saveAs })`, which replaces the four save sites.
- **Upload pause.**
  - `PendingAttachment` gains `paused` + `setPaused`. The upload loop waits (abortable) before
    reading the next chunk while paused, so Pause bites within one chunk: ≤ 8 MiB uncapped, about
    0.5 s under a cap.
  - `handleSend`'s wait loop already keeps waiting until every pending is ready or failed, so the
    message simply waits.
- **Settings.** `AppSettings` gains `download_dir: String` (empty means the OS default) and
  `ask_download_location: bool`, both `#[serde(default)]` (a `config.rs` change, then a napi build).
  `loadSettings` sends them to main the same way as the download cap.

### Community server (small, recommended)

The abandoned-upload sweep deletes `uploading` rows by **`created_at` > 1 h**
(`kUploadingTimeoutSeconds`). With pause, and with slow capped uploads, an upload that is still
alive can be deleted underneath the client, and its next PATCH gets a 404. The fix is to sweep on
**last PATCH activity** instead: stamp the row when a PATCH lands and sweep rows idle for more than
1 h. Add an e2e check in `e2e.py`. Until that ships, a 404 on resume reads "Upload expired on the
server" rather than a raw HTTP error.

## Phases

Each phase leaves `main` coherent and ships on its own.

1. **Downloads.**
   - The main-process manager: plain + encrypted, pause/resume/cancel/retry, the folder and unique
     names, sanitisation, persistence, quit → paused.
   - IPC and preload, the Settings section + `config.rs` fields.
   - Move the four save sites to `startDownload`. Their buffered `netFetch` → `fs.writeFile` path
     goes away, but `netFetch` GET stays for "Copy image".
   - The title-bar button (ring, dot, arrow drop, `Ctrl/Cmd+J`) and the panel with the
     **Downloads** tab: every row state, thumbnails, the right-click menu (Open · Show in folder ·
     Remove), the risky-file confirm, the footer and the empty states.
2. **Uploads.** The upload history in `transfersStore`, the Uploads tab rows, upload pause, the
   community sweep fix + e2e.
3. **Polish.**
   - Taskbar / dock progress.
   - Go to message: needs a store-level jump request
     (`chatStore.requestJump(serverId, channelId, messageId)`) consumed by `ChatPanel`, since
     `jumpToMessage` is local to the panel today.
   - A quit confirmation when an upload is in flight (quitting loses the message).

## Verification

- **Each phase**: tsc web + node 0; napi build + `cargo test --lib` when native changes.
- **Manager**: an Electron harness (like the speed-cap one) against a local HTTPS server that
  imitates the attachment endpoints. It covers:
  - Range resume (206), and a server that ignores Range (200 → restart)
  - Pause and resume mid-file, and cancel deleting the `.part`
  - Name collisions and a table of filename-sanitisation cases (`../x`, `CON.txt`, trailing dots,
    long and Unicode names)
  - An encrypted file sealed with `attachmentCrypto`: resume mid-window
  - The speed cap holding
  - Persistence: quit mid-download → relaunch → paused at the right byte count
- **UI**: preview-harness screenshots of every row state, both tabs and the empty states, in every
  theme.
- **Community**: build + e2e with the new sweep check (phase 2).
- **Live**: a real community server. A large download with pause/resume across a restart, a capped
  upload with pause, a Save-as, and Open / Show in folder on Linux and Windows.

## Risks and open points

- **Windows paths**: `MAX_PATH` with deep download folders. Cap the final path length and shorten
  the name before the extension.
- **Concurrent downloads**: no queue. Chromium's per-host socket pool (6) already limits how many
  run against one server, and inline image loads share that pool. Revisit if a big batch starves
  chat images.
- **Saved files are plaintext**: a file saved from an encrypted channel is plaintext on disk. That's
  inherent to saving; the panel doesn't change it.
- **Not handled**: "Ask where to save each file" doesn't affect "Save as…", which always asks.
  Drag-and-drop out of the panel (dragging a finished file into another app) isn't planned.
