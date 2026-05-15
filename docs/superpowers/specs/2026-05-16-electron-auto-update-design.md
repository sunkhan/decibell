# Electron Auto-Update Design

**Date:** 2026-05-16
**Status:** Approved
**Owner:** sunkhan

## Goal

Replace the current "download a new installer and reinstall on every release" loop with an in-app update mechanism. On platforms that support in-place upgrades (Windows NSIS, Linux AppImage), the client checks for a new version on launch, downloads silently in the background, installs on next quit, and surfaces a one-click "Restart" affordance. On other install paths (`.deb`, `.pacman`, AUR, macOS `.dmg`), the client surfaces a "new version available" notice that links to the release page — the user remains in control of the package-manager-driven upgrade.

## Non-goals

- **Code signing.** Shipping unsigned for v1. electron-updater verifies download integrity via SHA-512 from `latest.yml` regardless of signing, so updates work; users keep seeing SmartScreen warnings on first install just like today. Defer cert purchase until userbase pressure exists.
- **Delta updates.** electron-updater can produce blockmap-based incremental patches, but we'll just ship full installer/AppImage downloads in v1. Blockmaps are still generated and uploaded because they're free, but we won't optimize.
- **Beta / staging channels.** Single `latest` channel only.
- **Periodic recheck after launch.** One check 5 seconds after first window load, plus a manual "Check for updates" button. Always-on users who keep the app open for days can press the manual button.
- **Auto-update for `.deb`/`.pacman`/AUR/macOS.** Users on these paths update through their respective package manager (apt, pacman, AUR helper) or by manually replacing the `.dmg` install. The app only tells them a newer version exists.

## Architecture overview

```
┌───────────────────────────────┐                    ┌────────────────────┐
│  GitHub Releases (ev<X.Y.Z>)  │                    │  electron-builder  │
│  - Decibell-X.Y.Z-x64.exe     │  ← uploaded by ──  │  --publish=never   │
│  - Decibell-X.Y.Z-x64.AppImage│                    │  in CI on ev* tag  │
│  - latest.yml                 │     (metadata      │  (publish: block   │
│  - latest-linux.yml           │      generated     │   added so meta is │
│  - *.blockmap                 │      locally,      │   written to       │
│                               │      uploaded via  │   release/ even    │
│                               │      softprops)    │   though not       │
│                               │                    │   pushed)          │
└───────────────┬───────────────┘                    └────────────────────┘
                │
                │  HTTPS (no auth — public repo)
                ▼
┌──────────────────────────────────────────────────────────────────────┐
│  Main process — electron/main/update.ts                              │
│  - detectMode() → "self-update" | "notify-only" | "disabled"         │
│  - wires electron-updater event listeners to broadcast               │
│  - exposes initUpdater / kickoffInitialCheck / manualCheck /         │
│    quitAndInstall / getSnapshot                                      │
└────────────────────────────────────────┬─────────────────────────────┘
                                         │  "decibell:event" name=update_status
                                         ▼
┌──────────────────────────────────────────────────────────────────────┐
│  Renderer — updateStore (Zustand)                                    │
│  - AboutTab: version + status line + action button                   │
│  - UserPanel: persistent chip when downloaded && self-update         │
└──────────────────────────────────────────────────────────────────────┘
```

Single library (`electron-updater`), single event stream, branching collapses to one boolean (`autoDownload` and `autoInstallOnAppQuit`) at boot.

## Update source

GitHub Releases via `electron-updater`'s built-in GitHub provider.

- Public repo, anonymous fetch — no token shipped in the client.
- `electron-updater` calls `https://api.github.com/repos/sunkhan/decibell/releases/latest`, parses the JSON, finds the asset named `latest.yml` (or `latest-linux.yml`), downloads and parses it, then semver-compares the `version:` field against `app.getVersion()`.
- "Latest" on GitHub means most recent `published_at`, **not** highest semver. Operationally fine because we publish in chronological order — but a flagged invariant: never backport-tag an older branch as a fresh release.
- The `ev` tag prefix is purely a CI trigger and doesn't appear in the comparison. electron-updater reads `version:` from `latest.yml`, which electron-builder fills in from `package.json#version`.

## Platform scope

| Install variant   | Detection                                | Mode          | Behavior                                          |
|-------------------|------------------------------------------|---------------|---------------------------------------------------|
| Windows NSIS      | `process.platform === "win32"`           | `self-update` | Full background download + install on quit.       |
| Linux AppImage    | `process.platform === "linux"` + `process.env.APPIMAGE` set | `self-update` | Full background download + in-place replace.      |
| Linux .deb/.pacman| `linux` without `APPIMAGE`               | `notify-only` | AboutTab surfaces "Open release page". No chip.   |
| macOS .dmg        | `process.platform === "darwin"`          | `notify-only` | Same notify-only flow.                            |
| Dev (`npm run dev`)| `!app.isPackaged`                       | `disabled`    | AboutTab shows "Updates disabled in development". |

Mode is detected once at boot and persists for the session.

**AppImage caveat:** in-place replace requires write permission to the directory holding the `.AppImage`. If the user dropped it in `/opt/` or another root-owned location, the replace fails and electron-updater emits an `error` event. We surface that error verbatim; user can move the AppImage somewhere writable or download manually.

**Castlabs note:** electron-updater uses Squirrel.Windows on Win32. Castlabs ships a stock-Squirrel-compatible binary, so the platform autoUpdater layer is unaffected by the Widevine fork.

## Lifecycle

1. **Boot.** `initUpdater()` runs after `app.whenReady()`. Detects mode, sets `autoDownload` and `autoInstallOnAppQuit` flags, registers listeners. If mode is `disabled`, it returns early without attaching anything.
2. **Initial check.** First `BrowserWindow` fires `did-finish-load`. We schedule `kickoffInitialCheck()` 5 seconds after that — gives the login flow and websocket handshake room to breathe before competing for network.
3. **Manual check.** AboutTab's "Check now" button → `invoke("update:check")` → `autoUpdater.checkForUpdates()`. Button disables while `status.state` is `checking` or `downloading`.
4. **Update available.** In `self-update` mode, electron-updater auto-starts the download. In `notify-only` mode, it just emits `update-available` and stops.
5. **Download progress.** `download-progress` events stream percentage; renderer renders the percentage inline in AboutTab. No progress in `notify-only` (no download happens).
6. **Downloaded.** Status flips to `downloaded`. In `self-update` mode, the persistent chip mounts in UserPanel. The chip's onClick goes straight to `invoke("update:quitAndInstall")` — no detour through Settings. The "Restart" button in AboutTab does the same thing.
7. **Quit.** With `autoInstallOnAppQuit = true`, electron-updater spawns the installer in the background as `app.quit()` exits. User relaunches → new version.

## CI / electron-builder changes

### `electron-builder.yml`

Add a `publish` block. Without it, electron-builder doesn't know where the update manifest *points*, so the URLs it embeds in `latest.yml` are wrong.

```yaml
publish:
  provider: github
  owner: sunkhan
  repo: decibell
  # We still upload artifacts via softprops/action-gh-release in
  # electron-release.yml (clearer semantics, single source of truth
  # for what lands on the release). This block exists so the
  # metadata files (latest.yml, latest-linux.yml, *.blockmap) get
  # generated into release/ — electron-builder skips that work when
  # no publish provider is configured.
```

### `.github/workflows/electron-release.yml`

Two patches to the `Upload artifacts` step — add the new file patterns:

```yaml
path: |
  electron-client/release/*.AppImage
  electron-client/release/*.deb
  electron-client/release/*.pacman
  electron-client/release/*.exe
  electron-client/release/*.dmg
  electron-client/release/latest*.yml       # ← new
  electron-client/release/*.blockmap        # ← new
```

The release job's `files: artifacts/**/*` already picks up everything in `artifacts/`, so the manifests attach automatically. The `Package` step keeps `--publish=never` — explicitly DO NOT pass `GH_TOKEN` (would re-trigger the null-channel crash the existing comment documents).

### `package.json`

Fix the `homepage` mismatch:

```json
"homepage": "https://github.com/sunkhan/decibell"
```

And add the dependency:

```json
"dependencies": {
  "electron-updater": "^6.3.9",
  ...
}
```

(`electron-updater` ships as a regular dependency, not devDependency — it must be present in the packaged asar.)

## Main process

### `electron/main/update.ts` (new)

```ts
import { app, BrowserWindow } from "electron";
import { autoUpdater } from "electron-updater";

type UpdateMode = "self-update" | "notify-only" | "disabled";

type UpdateStatus =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "not-available"; checkedAt: number }
  | { state: "available"; version: string }
  | { state: "downloading"; pct: number; version: string }
  | { state: "downloaded"; version: string }
  | { state: "error"; message: string };

let mode: UpdateMode = "disabled";
let status: UpdateStatus = { state: "idle" };
let pendingVersion = "";

function detectMode(): UpdateMode {
  if (!app.isPackaged) return "disabled";
  if (process.platform === "win32") return "self-update";
  if (process.platform === "linux" && process.env.APPIMAGE) return "self-update";
  return "notify-only";
}

export function initUpdater(): void {
  mode = detectMode();
  if (mode === "disabled") {
    broadcast();
    return;
  }

  autoUpdater.autoDownload = mode === "self-update";
  autoUpdater.autoInstallOnAppQuit = mode === "self-update";
  autoUpdater.logger = console;

  autoUpdater.on("checking-for-update", () => setStatus({ state: "checking" }));
  autoUpdater.on("update-not-available", () =>
    setStatus({ state: "not-available", checkedAt: Date.now() }),
  );
  autoUpdater.on("update-available", (info) => {
    pendingVersion = info.version;
    setStatus({ state: "available", version: info.version });
  });
  autoUpdater.on("download-progress", (p) =>
    setStatus({ state: "downloading", pct: p.percent, version: pendingVersion }),
  );
  autoUpdater.on("update-downloaded", (info) =>
    setStatus({ state: "downloaded", version: info.version }),
  );
  autoUpdater.on("error", (e) =>
    setStatus({ state: "error", message: String(e?.message ?? e) }),
  );

  broadcast();
}

export function kickoffInitialCheck(): void {
  if (mode === "disabled") return;
  setTimeout(() => {
    autoUpdater.checkForUpdates().catch((err) => {
      console.error("[update] initial check failed:", err);
    });
  }, 5_000);
}

export async function manualCheck(): Promise<void> {
  if (mode === "disabled") return;
  await autoUpdater.checkForUpdates();
}

export function quitAndInstall(): void {
  if (mode !== "self-update") return;
  autoUpdater.quitAndInstall();
}

export function getSnapshot() {
  return { status, mode, currentVersion: app.getVersion() };
}

function setStatus(s: UpdateStatus): void {
  status = s;
  broadcast();
}

function broadcast(): void {
  const payload = { status, mode, currentVersion: app.getVersion() };
  for (const win of BrowserWindow.getAllWindows()) {
    win.webContents.send("decibell:event", { name: "update_status", payload });
  }
}
```

### `electron/main/index.ts` — wire it in

After existing `app.whenReady()` setup:

```ts
import { initUpdater, kickoffInitialCheck } from "./update";

// ... inside whenReady chain, after BrowserWindow created ...
initUpdater();
win.webContents.once("did-finish-load", () => {
  kickoffInitialCheck();
});
```

When a new BrowserWindow is created later (rare but possible — e.g., reopen after close on macOS), it'll get the current `status`/`mode` snapshot via the `broadcast()` triggered by the renderer subscribing on mount (see store init below).

## IPC layer

`electron/main/ipc.ts` registers four handlers:

```ts
import { manualCheck, quitAndInstall, getSnapshot } from "./update";
import { shell } from "electron";

ipcMain.handle("update:getStatus", () => {
  return getSnapshot();
});

ipcMain.handle("update:check", async () => {
  await manualCheck();
});

ipcMain.handle("update:quitAndInstall", () => {
  quitAndInstall();
});

ipcMain.handle("update:openReleasePage", () => {
  return shell.openExternal("https://github.com/sunkhan/decibell/releases/latest");
});
```

`getSnapshot()` returns `{ status, mode, currentVersion }` — same shape as the broadcast payload. AppLayout's listener mount invokes `update:getStatus` once on mount to populate the store with whatever state the main process is already in (covers the case where `initUpdater()`'s initial broadcast fired before the renderer attached its listener).

The renderer's existing event router (in `src/lib/ipc.ts` or equivalent) adds one case for `name === "update_status"` that pushes the payload into `updateStore`.

## Renderer state + UI

### `src/stores/updateStore.ts` (new)

```ts
import { create } from "zustand";

export type UpdateStatus =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "not-available"; checkedAt: number }
  | { state: "available"; version: string }
  | { state: "downloading"; pct: number; version: string }
  | { state: "downloaded"; version: string }
  | { state: "error"; message: string };

export type UpdateMode = "self-update" | "notify-only" | "disabled";

interface UpdateState {
  status: UpdateStatus;
  mode: UpdateMode;
  currentVersion: string;
  setFromEvent: (s: UpdateStatus, m: UpdateMode, v: string) => void;
}

export const useUpdateStore = create<UpdateState>((set) => ({
  status: { state: "idle" },
  mode: "disabled",
  currentVersion: "",
  setFromEvent: (status, mode, currentVersion) =>
    set({ status, mode, currentVersion }),
}));
```

### Event listener mount

In `src/layouts/AppLayout.tsx` — replaces the placeholder comment that currently says `// UpdateChecker (electron-updater) and ResizeHandles ... port with their own PRs`:

```ts
useEffect(() => {
  // Pull initial state — covers the case where initUpdater()'s
  // boot-time broadcast fired before this listener attached.
  invoke("update:getStatus").then((p) => {
    useUpdateStore.getState().setFromEvent(p.status, p.mode, p.currentVersion);
  });
  return onDecibellEvent("update_status", (p) => {
    useUpdateStore.getState().setFromEvent(p.status, p.mode, p.currentVersion);
  });
}, []);
```

### AboutTab section

Appended below the existing version display in `src/features/settings/tabs/AboutTab.tsx`. Single component, switch on `status.state`:

| State                | Status line                          | Button             |
|----------------------|--------------------------------------|--------------------|
| `idle`               | "You're up to date."                 | "Check now"        |
| `not-available`      | "You're up to date."                 | "Check now"        |
| `checking`           | "Checking for updates…"              | (disabled)         |
| `available` (self)   | "Update available — preparing download…" | (disabled)     |
| `available` (notify) | "<version> is available."            | "Open release page"|
| `downloading`        | "Downloading <version>… <pct>%"      | (disabled)         |
| `downloaded` (self)  | "Update ready: <version>"            | "Restart"          |
| `error`              | "Couldn't check: <error.message>"    | "Try again"        |
| (`mode = disabled`)  | "Updates are disabled in development."| (no button)       |

Buttons:
- `Check now` / `Try again` → `invoke("update:check")`
- `Restart` → `invoke("update:quitAndInstall")`
- `Open release page` → `invoke("update:openReleasePage")`

### Persistent chip — UserPanel

Mounts only when `status.state === "downloaded" && mode === "self-update"`. Slim pill inserted at the top of `UserPanel` (the bottom-left floating panel), above the existing avatar/username row.

```
┌──────────────────────────────────┐
│ ● Update ready — Restart  →      │  ← chip
├──────────────────────────────────┤
│ [avatar] gunhan                  │
│         online                   │
│  🎙   🎧   ⚙               ⏻     │  ← existing row
└──────────────────────────────────┘
```

Click → `invoke("update:quitAndInstall")` directly. No interstitial Settings open, no confirm modal. The chip is the affordance.

Style: small accent-mid background, accent-bright text, dot indicator using the `dropPulse` keyframe already used by the active server tile — visually consistent with other "ready/active" indicators in the app.

In `notify-only` mode, no chip ever mounts. Updates surface only inside AboutTab.

## Failure modes

| Failure                                  | Where it surfaces                  | Recovery                                          |
|------------------------------------------|------------------------------------|---------------------------------------------------|
| Network unreachable                      | `error` event → AboutTab error line| User clicks "Try again". No backoff schedule.     |
| GitHub API 5xx                           | `error` event → AboutTab error line| Same.                                             |
| `latest.yml` parse failure (corrupt CI)  | `error` event → AboutTab error line| Operationally a release-pipeline bug; we surface the error verbatim. |
| SHA-512 mismatch on downloaded blob      | `error` event → AboutTab error line| electron-updater discards the partial download. User retries via "Check now". |
| AppImage in read-only path               | `error` event with filesystem text | Surfaced verbatim. No special-case handling.      |
| Quit-and-install spawn fails             | electron-updater logs only         | App stays open. User can retry from AboutTab.     |

No silent retries, no escalating prompts. Every failure surfaces the underlying error message so the user understands what's broken.

## File-level changes

**New files:**
- `electron-client/electron/main/update.ts` (~110 LOC)
- `electron-client/src/stores/updateStore.ts` (~30 LOC)

**Modified files:**
- `electron-client/electron-builder.yml` — add `publish:` block
- `electron-client/package.json` — fix `homepage`, add `electron-updater` dependency
- `electron-client/electron/main/index.ts` — call `initUpdater()` and `kickoffInitialCheck()`
- `electron-client/electron/main/ipc.ts` — three new `ipcMain.handle` entries
- `electron-client/src/layouts/AppLayout.tsx` — replace placeholder comment with mounted listener
- `electron-client/src/features/settings/tabs/AboutTab.tsx` — append Updates section
- `electron-client/src/features/channels/UserPanel.tsx` — conditional chip at top
- `.github/workflows/electron-release.yml` — add `latest*.yml` + `*.blockmap` to upload glob

**LOC budget:** ~250 new, ~80 modified.

## Out of scope (deferred to later versions)

- Periodic recheck while the app is running.
- Multiple release channels (beta/stable).
- Delta updates (blockmap-based incremental patches). Files generated and uploaded so we can flip this on later without re-shipping older versions.
- Code signing (Windows / macOS).
- macOS / `.deb` / `.pacman` / AUR in-app upgrades.
- Update size in UI ("Downloading 35 MB…"). v1 just shows percent.
- "Don't ask me again for this version" — there's no nag in v1, so nothing to suppress.
