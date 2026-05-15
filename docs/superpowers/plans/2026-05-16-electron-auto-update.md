# Electron Auto-Update Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire `electron-updater` into the Electron client so packaged installs check GitHub Releases on launch, self-update on Win NSIS + Linux AppImage, and notify-only on `.deb`/`.pacman`/AUR/`.dmg`.

**Architecture:** A single `UpdateController` in the main process owns all `electron-updater` interaction, broadcasts state to the renderer via the existing `decibell:event` channel, and exposes 4 IPC commands. The renderer mirrors state in a Zustand store, surfaces it through the AboutTab Updates section, and mounts a persistent "Update ready — Restart" chip in `UserPanel` when a download is ready (self-update mode only). Boot-time platform detection collapses branching to a single `mode` variable.

**Tech Stack:** Electron 33 (Castlabs Widevine fork), `electron-updater` v6, `electron-builder` v25 (GitHub publish provider for `latest.yml` generation), Zustand state store, React 18.

**Spec:** `docs/superpowers/specs/2026-05-16-electron-auto-update-design.md` (commit `6375069`).

---

## Spec deviation noted before starting

The spec uses `invoke("update:check")` in renderer code samples. The codebase's `invoke()` (in `src/lib/ipc.ts`) is a Tauri-compat shim that routes everything through `ipcRenderer.invoke("decibell:invoke", method, args)` → `callCommand()` → Rust addon. That path is for native commands and won't reach an `ipcMain.handle("update:check")` registered in the main process.

The actual renderer-side calls must use a new namespace on the preload bridge: `window.decibell.update.{getStatus,check,quitAndInstall,openReleasePage}()`, mirroring the existing `window.decibell.dialog` / `window.decibell.window` patterns. Task 6 adds this namespace; downstream renderer tasks (9, 10, 11) call through it.

---

## File-level map

**New files**

| Path | Responsibility |
|---|---|
| `electron-client/electron/main/update.ts` | `UpdateController` — owns all `electron-updater` interaction, mode detection, broadcast helper. |
| `electron-client/src/stores/updateStore.ts` | Zustand store mirroring main-process state for the renderer. |

**Modified files**

| Path | Change |
|---|---|
| `electron-client/package.json` | Add `electron-updater` dep; fix `homepage` URL. |
| `electron-client/electron-builder.yml` | Add `publish:` block (provider + owner + repo). |
| `electron-client/electron/main/ipc.ts` | Add 4 `ipcMain.handle` entries for the update channel names. |
| `electron-client/electron/main/index.ts` | Call `initUpdater()` in `whenReady`; call `kickoffInitialCheck()` on first `did-finish-load`. |
| `electron-client/electron/preload/index.ts` | Add `window.decibell.update` namespace. |
| `electron-client/src/types/global.d.ts` | Type declarations for the new namespace. |
| `electron-client/src/layouts/AppLayout.tsx` | Replace the leftover `// UpdateChecker (electron-updater) ... port with their own PRs` placeholder with the listener + initial-snapshot pull. |
| `electron-client/src/features/settings/tabs/AboutTab.tsx` | Append "Updates" section below the existing version display. |
| `electron-client/src/features/channels/UserPanel.tsx` | Insert conditional "Update ready — Restart" chip at the top of the panel. |
| `.github/workflows/electron-release.yml` | Add `latest*.yml` + `*.blockmap` to the artifact upload glob. |

---

## Task list

### Task 1: Install `electron-updater` and fix `package.json` `homepage`

**Files:**
- Modify: `electron-client/package.json`

- [ ] **Step 1: Update `package.json` dependencies block + `homepage`**

Edit `electron-client/package.json`:

Change line 6 from:
```json
  "homepage": "https://github.com/decibell/decibell",
```
to:
```json
  "homepage": "https://github.com/sunkhan/decibell",
```

In the `"dependencies": { ... }` block (line 27), add `electron-updater` (alphabetical order keeps it between `emoji-regex` and `lucide-react`):
```json
    "emoji-regex": "^10.6.0",
    "electron-updater": "^6.3.9",
    "lucide-react": "^1.8.0",
```

- [ ] **Step 2: Install**

Run from `electron-client/`:
```sh
npm install
```
Expected: `electron-updater@6.3.x` added to `node_modules`, `package-lock.json` updated. No errors.

- [ ] **Step 3: Commit**

```sh
git add electron-client/package.json electron-client/package-lock.json
git commit -m "build(deps): add electron-updater + fix homepage URL

electron-updater pulls latest.yml from GitHub Releases and runs the
in-place update flow on Windows NSIS + Linux AppImage. Homepage URL
was pointing at a non-existent decibell/decibell repo; corrected to
the actual remote at sunkhan/decibell so electron-builder can derive
the publish target if no explicit publish block is set.
"
```

---

### Task 2: Add `publish` block to `electron-builder.yml`

**Files:**
- Modify: `electron-client/electron-builder.yml`

- [ ] **Step 1: Insert publish block after the `appId`/`productName`/`copyright`/`directories` block, before `electronDownload`**

Find this section near the top:
```yaml
appId: com.decibell.app
productName: Decibell
copyright: Copyright © 2026 Decibell
directories:
  output: release
  buildResources: resources

# We use the Castlabs Widevine-enabled Electron fork
```

Insert immediately after `directories:` block (before the `# We use the Castlabs ...` comment):
```yaml

# Update metadata target. Without a publish block configured,
# electron-builder skips generating the latest.yml / latest-linux.yml
# / *.blockmap files that electron-updater needs to discover newer
# versions. The CI still uploads artifacts via softprops/action-gh-
# release (clearer semantics; single source of truth for what lands
# on the release page), so we keep --publish=never on the package
# step. This block exists purely to coerce electron-builder into
# emitting the metadata files into release/ as part of the build.
publish:
  provider: github
  owner: sunkhan
  repo: decibell
```

- [ ] **Step 2: Commit**

```sh
git add electron-client/electron-builder.yml
git commit -m "build(electron-builder): add github publish block for latest.yml generation

electron-builder only emits latest.yml / latest-linux.yml /
*.blockmap into the output dir when a publish provider is configured
— without these files, electron-updater has nothing to read on
launch. We still keep --publish=never in CI; the metadata is
uploaded via the existing softprops/action-gh-release step.
"
```

---

### Task 3: Create the `UpdateController` (`electron/main/update.ts`)

**Files:**
- Create: `electron-client/electron/main/update.ts`

- [ ] **Step 1: Write the full controller file**

Create `electron-client/electron/main/update.ts` with this content:

```ts
import { app, BrowserWindow } from "electron";
import { autoUpdater } from "electron-updater";

// Owner of all electron-updater interaction. One state machine in the
// main process, broadcast to the renderer on every transition, with a
// single boot-time mode detection that decides whether we auto-
// download / auto-install or just notify.
//
// Lifecycle:
//   initUpdater()           — wires listeners, sets autoDownload +
//                             autoInstallOnAppQuit flags.
//   kickoffInitialCheck()   — schedules the first checkForUpdates() 5s
//                             after first window load.
//   manualCheck()           — bound to the AboutTab "Check now" button.
//   quitAndInstall()        — bound to the AboutTab "Restart" button +
//                             the persistent UserPanel chip.
//   getSnapshot()           — pulled by the renderer on AppLayout mount
//                             so a late-arriving listener doesn't miss
//                             the boot-time broadcast.

export type UpdateMode = "self-update" | "notify-only" | "disabled";

export type UpdateStatus =
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
  // !isPackaged: in dev, electron-updater throws on first
  // checkForUpdates() because the bundled app-update.yml doesn't
  // exist. Disable explicitly so the renderer can render "Updates
  // disabled in development" and we don't surface a confusing error.
  if (!app.isPackaged) return "disabled";
  // Windows: NSIS installer → electron-updater drives Squirrel for
  // in-place upgrade. Castlabs Electron 33 ships a stock-Squirrel-
  // compatible binary so no fork-specific path is needed.
  if (process.platform === "win32") return "self-update";
  // Linux AppImage sets APPIMAGE to the absolute path of the running
  // .AppImage file. Without this env var, the running install is a
  // .deb / .pacman / AUR build whose upgrade is managed by the
  // distro's package manager.
  if (process.platform === "linux" && process.env.APPIMAGE) {
    return "self-update";
  }
  // macOS .dmg, Linux .deb / .pacman / AUR — surface only "new
  // version available", don't try to download.
  return "notify-only";
}

export function initUpdater(): void {
  mode = detectMode();
  console.log(`[update] mode=${mode}, version=${app.getVersion()}`);
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
  // 5s gives the login flow + websocket handshake room to land
  // before we add another network operation.
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

export function getSnapshot(): {
  status: UpdateStatus;
  mode: UpdateMode;
  currentVersion: string;
} {
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

- [ ] **Step 2: Run `tsc` to verify it compiles**

From `electron-client/`:
```sh
npm run build:tsc
```
Expected: no errors. Compiles `update.ts` to `dist/electron/main/update.js`.

- [ ] **Step 3: Commit**

```sh
git add electron-client/electron/main/update.ts
git commit -m "feat(update): UpdateController — electron-updater wiring + mode detection

Single owner for all electron-updater interaction. Boot-time mode
detection collapses platform branching to one variable (self-update
on Win NSIS + Linux AppImage; notify-only on .deb/.pacman/.dmg;
disabled in dev). Listeners translate electron-updater events to a
single UpdateStatus union and broadcast over the existing
'decibell:event' channel. Public surface kept tiny: init /
kickoffInitialCheck / manualCheck / quitAndInstall / getSnapshot.
"
```

---

### Task 4: Register IPC handlers for the 4 update commands

**Files:**
- Modify: `electron-client/electron/main/ipc.ts`

- [ ] **Step 1: Rewrite `ipc.ts` to add the handlers**

The current `ipc.ts` only registers `decibell:invoke` (the Tauri-shim dispatch). We're piggybacking the update handlers onto the existing `registerInvokeHandler` function — keeping all main-process IPC registration centralized.

Replace the entire file contents with:

```ts
import { ipcMain, shell } from "electron";
import { callCommand } from "./addon";
import {
  manualCheck,
  quitAndInstall,
  getSnapshot,
} from "./update";

export function registerInvokeHandler(): void {
  // Existing native-command bridge. Renderer's invoke() from
  // src/lib/ipc.ts routes here, then dispatches into the Rust addon.
  ipcMain.handle(
    "decibell:invoke",
    async (_event, method: string, args: unknown) => {
      const result = callCommand(method, args);
      // Napi async fns return Promises; sync fns return values. await
      // collapses both — contextBridge structured-clones whatever resolves.
      return await Promise.resolve(result);
    },
  );

  // Auto-update commands. Renderer reaches these via the
  // window.decibell.update.* namespace exposed in the preload bridge —
  // NOT via the Tauri-shim invoke(), which would route to the Rust
  // addon and miss these handlers entirely.
  ipcMain.handle("decibell:update:getStatus", () => {
    return getSnapshot();
  });
  ipcMain.handle("decibell:update:check", async () => {
    await manualCheck();
  });
  ipcMain.handle("decibell:update:quitAndInstall", () => {
    quitAndInstall();
  });
  ipcMain.handle("decibell:update:openReleasePage", async () => {
    await shell.openExternal(
      "https://github.com/sunkhan/decibell/releases/latest",
    );
  });
}
```

- [ ] **Step 2: Verify tsc**

```sh
npm run build:tsc
```
Expected: no errors.

- [ ] **Step 3: Commit**

```sh
git add electron-client/electron/main/ipc.ts
git commit -m "feat(update): 4 IPC handlers under decibell:update:* namespace

Renderer reaches these via the window.decibell.update preload-bridge
namespace (added in the next commit), not via the native-command
invoke() shim — keeps the native-command dispatch path separate from
main-process-only command channels.
"
```

---

### Task 5: Wire `initUpdater` + `kickoffInitialCheck` into `electron/main/index.ts`

**Files:**
- Modify: `electron-client/electron/main/index.ts`

- [ ] **Step 1: Add the import**

In `electron-client/electron/main/index.ts`, find line 15:
```ts
import { startMediaServer, stopMediaServer, getMediaServerPort } from "./mediaServer";
```

Add immediately below:
```ts
import { initUpdater, kickoffInitialCheck } from "./update";
```

- [ ] **Step 2: Call `initUpdater()` inside `app.whenReady().then(...)`**

Find line 537 (the existing `createWindow();` call inside the `whenReady` chain). Replace:

```ts
  createWindow();
  // initAddon must come AFTER createWindow so the bus broadcaster has
  // a window to dispatch to. Events fired before the renderer attaches
  // its 'decibell:event' listener are dropped — Electron only buffers
  // webContents.send calls after did-finish-load. The PR2 demo emit
  // sleeps 3s for exactly this reason.
  initAddon();
```

with:

```ts
  createWindow();
  // initAddon must come AFTER createWindow so the bus broadcaster has
  // a window to dispatch to. Events fired before the renderer attaches
  // its 'decibell:event' listener are dropped — Electron only buffers
  // webContents.send calls after did-finish-load. The PR2 demo emit
  // sleeps 3s for exactly this reason.
  initAddon();
  initUpdater();
```

- [ ] **Step 3: Schedule the initial check on first `did-finish-load`**

In the same file, find the existing `did-finish-load` handler inside `createWindow()` (around line 301):

```ts
  mainWindow.webContents.once("did-finish-load", () => {
    void dumpGpuInfo();
    if (pendingDeepLinks.length > 0 && mainWindow) {
      for (const url of pendingDeepLinks) {
        mainWindow.webContents.send("decibell:event", {
          name: "deep_link_received",
          payload: { url },
        });
      }
      pendingDeepLinks.length = 0;
    }
  });
```

Replace with:

```ts
  mainWindow.webContents.once("did-finish-load", () => {
    void dumpGpuInfo();
    if (pendingDeepLinks.length > 0 && mainWindow) {
      for (const url of pendingDeepLinks) {
        mainWindow.webContents.send("decibell:event", {
          name: "deep_link_received",
          payload: { url },
        });
      }
      pendingDeepLinks.length = 0;
    }
    // Schedule the first auto-update check. kickoffInitialCheck has
    // its own internal 5s delay so the login + websocket handshake
    // gets to land first.
    kickoffInitialCheck();
  });
```

- [ ] **Step 4: Verify tsc**

```sh
npm run build:tsc
```
Expected: no errors.

- [ ] **Step 5: Commit**

```sh
git add electron-client/electron/main/index.ts
git commit -m "feat(update): wire initUpdater + kickoffInitialCheck into main entry

initUpdater runs once after createWindow + initAddon so the
electron-updater listeners are in place before the window can ask
about update state. kickoffInitialCheck rides the first did-finish-
load — its own internal 5s timer holds the actual checkForUpdates()
back until after the login + websocket handshake have settled.
"
```

---

### Task 6: Preload bridge — add `window.decibell.update` namespace

**Files:**
- Modify: `electron-client/electron/preload/index.ts`
- Modify: `electron-client/src/types/global.d.ts`

- [ ] **Step 1: Define payload types and add the namespace in `preload/index.ts`**

Open `electron-client/electron/preload/index.ts`. Find the `CaptureSource` type definition (around line 60):

```ts
type CaptureSource = {
  id: string;
  name: string;
  displayId: string;
  appIcon: string;
  thumbnail: string;
  kind: "screen" | "window";
};
```

Add immediately above it:

```ts
type UpdateStatus =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "not-available"; checkedAt: number }
  | { state: "available"; version: string }
  | { state: "downloading"; pct: number; version: string }
  | { state: "downloaded"; version: string }
  | { state: "error"; message: string };

type UpdateMode = "self-update" | "notify-only" | "disabled";

type UpdateSnapshot = {
  status: UpdateStatus;
  mode: UpdateMode;
  currentVersion: string;
};

```

Then find the `capture: { ... }` namespace inside `contextBridge.exposeInMainWorld("decibell", { ... })` (around line 246) — it's the last namespace before the closing `});`. After the closing `}` of `capture`, add:

```ts
  update: {
    /// Pull the current main-process snapshot. Called on AppLayout
    /// mount to cover the case where initUpdater()'s boot-time
    /// broadcast fired before the renderer attached its listener.
    getStatus: (): Promise<UpdateSnapshot> =>
      ipcRenderer.invoke("decibell:update:getStatus") as Promise<UpdateSnapshot>,
    /// Manually trigger a check. Resolves once the autoUpdater promise
    /// resolves — the actual update_status events stream over
    /// 'decibell:event' as usual, so a caller can fire-and-forget.
    check: (): Promise<void> =>
      ipcRenderer.invoke("decibell:update:check") as Promise<void>,
    /// Quit the app and install the downloaded update. Caller must
    /// have already seen status.state === "downloaded". No-op when
    /// mode !== "self-update".
    quitAndInstall: (): Promise<void> =>
      ipcRenderer.invoke("decibell:update:quitAndInstall") as Promise<void>,
    /// Open the GitHub releases page in the user's default browser.
    /// Used by the notify-only mode action button.
    openReleasePage: (): Promise<void> =>
      ipcRenderer.invoke("decibell:update:openReleasePage") as Promise<void>,
  },
```

- [ ] **Step 2: Mirror the type in `src/types/global.d.ts`**

Open `electron-client/src/types/global.d.ts`. Find the `capture: { ... }` block at the end of the `decibell: { ... }` interface (around line 95):

```ts
      capture: {
        listSources: (opts?: {
          thumbnailWidth?: number;
          thumbnailHeight?: number;
        }) => Promise<CaptureSource[]>;
        setNextSource: (id: string | null) => Promise<void>;
      };
    };
  }
}
```

Add the `update` namespace immediately after `capture`'s closing `};`:

```ts
      capture: {
        listSources: (opts?: {
          thumbnailWidth?: number;
          thumbnailHeight?: number;
        }) => Promise<CaptureSource[]>;
        setNextSource: (id: string | null) => Promise<void>;
      };
      update: {
        getStatus: () => Promise<{
          status:
            | { state: "idle" }
            | { state: "checking" }
            | { state: "not-available"; checkedAt: number }
            | { state: "available"; version: string }
            | { state: "downloading"; pct: number; version: string }
            | { state: "downloaded"; version: string }
            | { state: "error"; message: string };
          mode: "self-update" | "notify-only" | "disabled";
          currentVersion: string;
        }>;
        check: () => Promise<void>;
        quitAndInstall: () => Promise<void>;
        openReleasePage: () => Promise<void>;
      };
    };
  }
}
```

- [ ] **Step 3: Verify tsc (both projects)**

```sh
npm run typecheck
```
Expected: no errors on either `tsconfig.web.json` or `tsconfig.node.json`.

- [ ] **Step 4: Commit**

```sh
git add electron-client/electron/preload/index.ts electron-client/src/types/global.d.ts
git commit -m "feat(update): preload bridge exposes window.decibell.update.{getStatus,check,quitAndInstall,openReleasePage}

Mirrors the existing window.decibell.dialog / window.decibell.window
namespaces — main-process-only commands get their own preload-bridge
slot rather than routing through the Tauri-shim invoke() which is
reserved for native-addon commands.
"
```

---

### Task 7: Create the renderer Zustand store

**Files:**
- Create: `electron-client/src/stores/updateStore.ts`

- [ ] **Step 1: Write the store**

Create `electron-client/src/stores/updateStore.ts`:

```ts
import { create } from "zustand";

// Mirror of the main-process UpdateController state. AppLayout
// subscribes to 'update_status' broadcasts on the 'decibell:event'
// channel and pushes payloads into this store. Components elsewhere
// (AboutTab, UserPanel) read from it directly.

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

- [ ] **Step 2: Verify tsc**

```sh
npx tsc -p tsconfig.web.json --noEmit
```
Expected: no errors.

- [ ] **Step 3: Commit**

```sh
git add electron-client/src/stores/updateStore.ts
git commit -m "feat(update): updateStore — Zustand mirror of main-process state

Single source of truth for the renderer. AppLayout pushes status
broadcasts into it; AboutTab + UserPanel read from it for their
respective UI surfaces.
"
```

---

### Task 8: Mount the event listener in `AppLayout.tsx`

**Files:**
- Modify: `electron-client/src/layouts/AppLayout.tsx`

- [ ] **Step 1: Replace the placeholder comment with a real listener**

Open `electron-client/src/layouts/AppLayout.tsx`. Replace the entire file contents (it's currently 23 lines including the placeholder comment) with:

```tsx
import { useEffect } from "react";
import { Outlet } from "react-router-dom";
import Titlebar from "./Titlebar";
import ToastStack from "../components/ToastStack";
import { listen } from "../lib/ipc";
import { useUpdateStore, type UpdateStatus, type UpdateMode } from "../stores/updateStore";

// Always-on chrome wrapper. Both /login and / sit inside this layout
// so the custom Titlebar (with min/max/close) stays present from the
// moment the window opens. The outlet renders the active route's
// content beneath the titlebar. ToastStack is also mounted here so
// notifications appear on /login as well as /.

interface UpdateEventPayload {
  status: UpdateStatus;
  mode: UpdateMode;
  currentVersion: string;
}

export default function AppLayout() {
  useEffect(() => {
    // Pull the current snapshot first — covers the case where
    // initUpdater()'s boot-time broadcast fired before this listener
    // attached. After this, every subsequent transition arrives via
    // the 'update_status' event below.
    window.decibell.update.getStatus().then((snap) => {
      useUpdateStore.getState().setFromEvent(
        snap.status,
        snap.mode,
        snap.currentVersion,
      );
    });

    let unlistenFn: (() => void) | null = null;
    listen<UpdateEventPayload>("update_status", (event) => {
      const p = event.payload;
      useUpdateStore.getState().setFromEvent(
        p.status,
        p.mode,
        p.currentVersion,
      );
    }).then((u) => {
      unlistenFn = u;
    });
    return () => {
      if (unlistenFn) unlistenFn();
    };
  }, []);

  return (
    <div className="relative flex h-screen w-screen flex-col bg-bg-primary text-text-primary">
      <Titlebar />
      <div className="flex min-h-0 flex-1">
        <Outlet />
      </div>
      <ToastStack />
    </div>
  );
}
```

- [ ] **Step 2: Verify tsc**

```sh
npx tsc -p tsconfig.web.json --noEmit
```
Expected: no errors.

- [ ] **Step 3: Commit**

```sh
git add electron-client/src/layouts/AppLayout.tsx
git commit -m "feat(update): mount update_status listener + initial-snapshot pull in AppLayout

Replaces the leftover '// UpdateChecker (electron-updater) ... port
with their own PRs' placeholder comment from the Tauri-era port.
Pulls the current main-process snapshot once on mount (covers the
boot-time-broadcast race) then subscribes to 'update_status' for
every subsequent transition.
"
```

---

### Task 9: Append the "Updates" section to `AboutTab`

**Files:**
- Modify: `electron-client/src/features/settings/tabs/AboutTab.tsx`

- [ ] **Step 1: Replace the file with the expanded version**

Open `electron-client/src/features/settings/tabs/AboutTab.tsx`. Replace the entire file with:

```tsx
import { useUpdateStore } from "../../../stores/updateStore";

export default function AboutTab() {
  const status = useUpdateStore((s) => s.status);
  const mode = useUpdateStore((s) => s.mode);
  const currentVersion = useUpdateStore((s) => s.currentVersion);

  return (
    <div className="flex flex-col gap-4">
      {/* App info card */}
      <div className="rounded-[10px] border border-border-divider bg-bg-light px-5 py-5">
        <div className="mb-4 flex items-center gap-3">
          <div className="flex h-10 w-10 items-center justify-center rounded-[10px] bg-gradient-to-br from-accent to-accent-bright">
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="white" strokeWidth="2.5" strokeLinecap="round">
              <path d="M9 18V5l12-2v13" />
              <circle cx="6" cy="18" r="3" fill="white" stroke="none" />
              <circle cx="18" cy="16" r="3" fill="white" stroke="none" />
            </svg>
          </div>
          <div>
            <div className="font-display text-[14px] font-semibold text-text-primary">Decibell</div>
            <div className="text-[12px] text-text-muted">Decentralized game chat</div>
          </div>
        </div>
        <div className="text-[12px] text-text-secondary">
          Version <span className="font-medium text-text-primary">{currentVersion || "0.6.4"}</span>
        </div>
      </div>

      {/* Updates card */}
      <div className="rounded-[10px] border border-border-divider bg-bg-light px-5 py-5">
        <div className="mb-3 text-[13px] font-semibold text-text-primary">Updates</div>
        <UpdateStatusRow status={status} mode={mode} />
      </div>
    </div>
  );
}

function UpdateStatusRow({
  status,
  mode,
}: {
  status: ReturnType<typeof useUpdateStore.getState>["status"];
  mode: ReturnType<typeof useUpdateStore.getState>["mode"];
}) {
  const onCheck = () => {
    window.decibell.update.check().catch((err) => {
      console.error("[update] check failed:", err);
    });
  };
  const onRestart = () => {
    window.decibell.update.quitAndInstall().catch((err) => {
      console.error("[update] quitAndInstall failed:", err);
    });
  };
  const onOpenReleasePage = () => {
    window.decibell.update.openReleasePage().catch((err) => {
      console.error("[update] openReleasePage failed:", err);
    });
  };

  if (mode === "disabled") {
    return (
      <div className="text-[12px] text-text-muted">
        Updates are disabled in development builds.
      </div>
    );
  }

  let line: React.ReactNode = null;
  let button: React.ReactNode = null;

  switch (status.state) {
    case "idle":
    case "not-available":
      line = <span className="text-text-secondary">You're up to date.</span>;
      button = (
        <PrimaryButton onClick={onCheck}>Check now</PrimaryButton>
      );
      break;
    case "checking":
      line = <span className="text-text-secondary">Checking for updates…</span>;
      button = <PrimaryButton disabled>Check now</PrimaryButton>;
      break;
    case "available":
      if (mode === "notify-only") {
        line = (
          <span className="text-text-primary">
            {status.version} is available.
          </span>
        );
        button = (
          <PrimaryButton onClick={onOpenReleasePage}>
            Open release page
          </PrimaryButton>
        );
      } else {
        line = (
          <span className="text-text-secondary">
            Update available — preparing download…
          </span>
        );
        button = <PrimaryButton disabled>Check now</PrimaryButton>;
      }
      break;
    case "downloading":
      line = (
        <span className="text-text-secondary">
          Downloading {status.version}… {Math.round(status.pct)}%
        </span>
      );
      button = <PrimaryButton disabled>Check now</PrimaryButton>;
      break;
    case "downloaded":
      line = (
        <span className="text-text-primary">
          Update ready: {status.version}
        </span>
      );
      button = <PrimaryButton onClick={onRestart}>Restart</PrimaryButton>;
      break;
    case "error":
      line = (
        <span className="text-error">Couldn't check: {status.message}</span>
      );
      button = <PrimaryButton onClick={onCheck}>Try again</PrimaryButton>;
      break;
  }

  return (
    <div className="flex items-center justify-between gap-3">
      <div className="min-w-0 flex-1 text-[12px]">{line}</div>
      <div className="shrink-0">{button}</div>
    </div>
  );
}

function PrimaryButton({
  children,
  onClick,
  disabled,
}: {
  children: React.ReactNode;
  onClick?: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className="rounded-md bg-accent px-3 py-1.5 text-[12px] font-semibold text-white transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
    >
      {children}
    </button>
  );
}
```

- [ ] **Step 2: Verify tsc**

```sh
npx tsc -p tsconfig.web.json --noEmit
```
Expected: no errors.

- [ ] **Step 3: Commit**

```sh
git add electron-client/src/features/settings/tabs/AboutTab.tsx
git commit -m "feat(update): AboutTab Updates section with status line + action button

State-driven row beneath the existing version card. Handles all 7
UpdateStatus states plus the notify-only/self-update mode branching
for the 'available' state (notify-only sends to the release page;
self-update shows 'preparing download…' as autoDownload kicks in).
Disabled-mode renders 'Updates are disabled in development builds.'
with no button.
"
```

---

### Task 10: Persistent chip in `UserPanel.tsx`

**Files:**
- Modify: `electron-client/src/features/channels/UserPanel.tsx`

- [ ] **Step 1: Add the import**

Open `electron-client/src/features/channels/UserPanel.tsx`. Find line 6:
```ts
import { useChatStore } from "../../stores/chatStore";
```

Add immediately below:
```ts
import { useUpdateStore } from "../../stores/updateStore";
```

- [ ] **Step 2: Read the chip-relevant state inside the component**

Find the `const channels = useChatStore(...)` line (around line 42). Add immediately after it (before the existing `const [showStats...` line on line 47):

```ts
  const updateStatus = useUpdateStore((s) => s.status);
  const updateMode = useUpdateStore((s) => s.mode);
  const showChip =
    updateStatus.state === "downloaded" && updateMode === "self-update";
  const handleChipRestart = () => {
    window.decibell.update.quitAndInstall().catch((err) => {
      console.error("[update] quitAndInstall failed:", err);
    });
  };
```

- [ ] **Step 3: Render the chip at the top of the panel's outer `<div>`**

Find the return statement (line 113):
```tsx
  return (
    <div className="rounded-[14px] border border-border bg-bg-light px-3 py-2.5 shadow-[0_4px_16px_rgba(0,0,0,0.5),0_0_0_1px_rgba(255,255,255,0.04)]">
      {connectedChannelId && (
```

Replace with:
```tsx
  return (
    <div className="rounded-[14px] border border-border bg-bg-light px-3 py-2.5 shadow-[0_4px_16px_rgba(0,0,0,0.5),0_0_0_1px_rgba(255,255,255,0.04)]">
      {showChip && (
        <button
          onClick={handleChipRestart}
          title={`Restart to update to ${updateStatus.state === "downloaded" ? updateStatus.version : ""}`}
          className="mb-2 flex w-full items-center gap-2 rounded-md bg-accent-soft px-2 py-1.5 text-left text-[12px] font-medium text-accent-bright transition-colors hover:bg-accent-mid"
        >
          <span className="h-1.5 w-1.5 rounded-full bg-accent-bright animate-[dropPulse_2.4s_ease-in-out_infinite]" />
          <span className="min-w-0 flex-1 truncate">
            Update ready
            {updateStatus.state === "downloaded" ? ` — ${updateStatus.version}` : ""}
          </span>
          <span className="shrink-0 text-[11px] font-semibold uppercase tracking-wide">
            Restart
          </span>
        </button>
      )}
      {connectedChannelId && (
```

- [ ] **Step 4: Verify tsc**

```sh
npx tsc -p tsconfig.web.json --noEmit
```
Expected: no errors.

- [ ] **Step 5: Commit**

```sh
git add electron-client/src/features/channels/UserPanel.tsx
git commit -m "feat(update): UserPanel chip — 'Update ready — Restart' (self-update mode only)

Slim pill at the top of UserPanel, visible only when status.state ===
'downloaded' && mode === 'self-update'. Click goes straight to
quitAndInstall — no Settings detour, no confirm modal. Notify-only
mode never sees the chip; updates there surface only inside AboutTab.
Dot indicator reuses the dropPulse keyframe used by the active
server tile for visual consistency.
"
```

---

### Task 11: Renderer build verification

**Files:**
- (no file changes)

- [ ] **Step 1: Run the full renderer build**

From `electron-client/`:
```sh
npm run build:renderer
```
Expected: builds without errors. `dist/renderer/` populated.

- [ ] **Step 2: Run the typecheck across both projects**

```sh
npm run typecheck
```
Expected: no errors on either `tsconfig.web.json` (renderer) or `tsconfig.node.json` (main + preload).

- [ ] **Step 3: Run the dev server briefly to confirm runtime mount**

```sh
npm run dev
```
Watch the terminal for `[update] mode=disabled, version=0.6.4` (the boot log line from `initUpdater`).
In the running app, open Settings → About. Expected:
- Version card shows "Version 0.6.4".
- Updates card shows "Updates are disabled in development builds." (no button).
No console errors related to update IPC or store.

Then kill with Ctrl-C.

- [ ] **Step 4: (no-op commit if everything passed)**

If any tsc fixes were needed, commit them. Otherwise, skip.

---

### Task 12: CI workflow — upload `latest*.yml` + `*.blockmap`

**Files:**
- Modify: `.github/workflows/electron-release.yml`

- [ ] **Step 1: Update the `Upload artifacts` glob**

Open `.github/workflows/electron-release.yml`. Find lines 167-177:

```yaml
      - name: Upload artifacts
        uses: actions/upload-artifact@v4
        with:
          name: decibell-${{ matrix.target_label }}
          path: |
            electron-client/release/*.AppImage
            electron-client/release/*.deb
            electron-client/release/*.pacman
            electron-client/release/*.exe
            electron-client/release/*.dmg
          if-no-files-found: ignore
```

Replace with:

```yaml
      - name: Upload artifacts
        uses: actions/upload-artifact@v4
        with:
          name: decibell-${{ matrix.target_label }}
          path: |
            electron-client/release/*.AppImage
            electron-client/release/*.deb
            electron-client/release/*.pacman
            electron-client/release/*.exe
            electron-client/release/*.dmg
            electron-client/release/latest*.yml
            electron-client/release/*.blockmap
          if-no-files-found: ignore
```

- [ ] **Step 2: Commit**

```sh
git add .github/workflows/electron-release.yml
git commit -m "ci(electron-release): upload latest*.yml + blockmap files

electron-updater on a packaged install fetches latest.yml /
latest-linux.yml from GitHub Releases to discover newer versions.
The metadata files are generated into release/ by electron-builder
when the publish block is configured (added two commits ago);
this step is what gets them attached to the GitHub Release the
softprops job creates.
"
```

---

### Task 13: Local packaged build — verify metadata generation

**Files:**
- (no file changes — verification only)

This task confirms the load-bearing assumption: that `electron-builder` with `publish:` configured + `--publish=never` actually writes `latest.yml` / `latest-linux.yml` / `*.blockmap` into `release/`. If it doesn't, the fallback is documented in Step 3.

- [ ] **Step 1: Run a packaged build locally**

From `electron-client/`:
```sh
npm run package
```
Expected: build completes. Output in `release/`.

- [ ] **Step 2: Verify the metadata files exist**

Run from `electron-client/`:
```sh
ls release/
```
Expected on a Windows host: `latest.yml`, `Decibell-0.6.4-x64.exe`, `Decibell-0.6.4-x64.exe.blockmap`.
Expected on a Linux host: `latest-linux.yml`, `Decibell-0.6.4-x86_64.AppImage`, `Decibell-0.6.4-x86_64.AppImage.blockmap`, `*.deb`, `*.pacman`.

Open `latest.yml` (or `latest-linux.yml`) and confirm it contains:
- `version: 0.6.4`
- A `files:` array with `url:` referencing the installer/AppImage filename, plus a `sha512:` hash.
- A top-level `path:` matching the primary installer/AppImage.

- [ ] **Step 3: If the metadata files are missing**

If `latest.yml` did NOT appear in `release/`, the spec's assumption is wrong. Two fallback paths:

**Fallback A — switch CI to `--publish=onTag` + `GH_TOKEN`:**
1. Open `.github/workflows/electron-release.yml`, find the `Package` step (line 158).
2. Change `run: npx electron-builder --publish=never` to:
   ```yaml
   env:
     GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
   run: npx electron-builder --publish=onTag
   ```
3. Remove the `release:` job at the bottom (electron-builder now creates the release itself; softprops would duplicate it).

**Fallback B — manual metadata generation:**
Investigate why electron-builder 25 isn't emitting the metadata locally. Possible cause: a missing `electronUpdaterCompatibility` field or a publish-provider variant. Open an issue against electron-builder or check their release notes for v25.

If fallback A is needed, document the change in this task and continue. Most likely the metadata files DO appear, in which case skip this step.

- [ ] **Step 4: (no-op commit unless a fallback was applied)**

If a fallback was needed and yaml changes were made, commit them. Otherwise, skip.

---

### Task 14: Manual E2E test pass

**Files:**
- (no file changes — verification only)

The full update flow can only be end-to-end-verified against an actual newer published release. This task documents the smoke tests we CAN do pre-shipping; the real end-to-end check happens on the release AFTER this one.

- [ ] **Step 1: Install the packaged build from Task 13**

Windows:
```sh
release\Decibell-0.6.4-x64.exe
```
Walk through the NSIS installer; let it install to the default location.

Linux:
```sh
chmod +x release/Decibell-0.6.4-x86_64.AppImage
./release/Decibell-0.6.4-x86_64.AppImage
```

- [ ] **Step 2: Verify boot-time mode detection logs**

In the running app, check the terminal where it launched (Linux) or the renderer DevTools console (Windows; open via Ctrl-Shift-I).
Expected log line from `initUpdater`:
- Windows: `[update] mode=self-update, version=0.6.4`
- Linux AppImage: `[update] mode=self-update, version=0.6.4`

- [ ] **Step 3: Verify the initial check runs and reports "up to date"**

Wait ~10 seconds after launch. Open Settings → About → Updates section. Expected:
- "You're up to date." line with the "Check now" button enabled.

In the terminal/DevTools, look for the `[update]` log lines from electron-updater showing it fetched `https://github.com/sunkhan/decibell/releases/latest/download/latest.yml` (or `latest-linux.yml`).

If the AboutTab shows "Couldn't check: <error>", investigate before shipping:
- 404 on `latest.yml`: the in-flight 0.6.4 release doesn't have the metadata attached yet (will be fixed when this PR's CI changes are live on the next release).
- network error: firewall / DNS — not a code issue.
- parse error: corrupted `latest.yml` — re-run Task 13.

- [ ] **Step 4: Verify manual check button**

Click "Check now". Expected:
- Button greys out.
- Status line briefly shows "Checking for updates…".
- Returns to "You're up to date." once the check completes.

- [ ] **Step 5: Verify chip is NOT showing**

Open the main view (close Settings). Expected:
- UserPanel at bottom-left does NOT show the "Update ready — Restart" chip (since no update is downloaded).

- [ ] **Step 6: Tag the release**

Once steps 1-5 pass:
```sh
# From the repo root
# Bump package.json#version first if not already done (e.g., 0.6.5)
git tag ev0.6.5
git push origin ev0.6.5
```
CI builds + publishes; metadata files get attached.

- [ ] **Step 7: On the NEXT release (ev0.6.6), verify the full update flow**

This is the only way to validate the download + install path end-to-end. When the next version ships:
1. Make sure your installed copy is 0.6.5.
2. Launch the app. Within 5-10 seconds the updater fetches `latest.yml`, sees `0.6.6`, and starts downloading.
3. Verify the AboutTab progresses through `checking → available → downloading X%`.
4. After the download finishes, verify the "Update ready — Restart" chip appears at the top of UserPanel.
5. Click the chip. App quits and installs.
6. Relaunch (manually or wait for Windows NSIS to spawn the new exe). Confirm `app.getVersion()` reports `0.6.6`.

If any step fails, file a follow-up; don't re-tag 0.6.6 to fix.

- [ ] **Step 8: Mark E2E task complete in TaskList**

After step 7 passes on the next release, mark this task complete.

---

## Self-review

**Spec coverage check:**

| Spec section | Task(s) |
|---|---|
| Update source (GH Releases + electron-updater) | Tasks 1, 2 |
| Platform scope (self-update vs notify-only vs disabled) | Task 3 (`detectMode`) |
| Lifecycle (boot, initial check, manual, download, downloaded, quit) | Tasks 3, 5 |
| CI / electron-builder changes (publish block + workflow upload) | Tasks 2, 12 |
| Main process — `electron/main/update.ts` | Task 3 |
| Main process — `electron/main/index.ts` wiring | Task 5 |
| IPC layer — 4 handlers | Task 4 |
| Preload bridge namespace + global.d.ts | Task 6 |
| Renderer state (`updateStore`) | Task 7 |
| AppLayout event listener | Task 8 |
| AboutTab section | Task 9 |
| UserPanel chip | Task 10 |
| Failure modes | Covered inline in Task 3 listeners + Task 9 error branch |
| Local build verification of metadata generation | Task 13 |
| Manual E2E pass | Task 14 |

**Type consistency check:** `UpdateStatus`, `UpdateMode`, `UpdateSnapshot` (and the same shape under the equivalent inline type in `global.d.ts`) all use the same `state` discriminator and the same field names (`pct`, `version`, `message`, `checkedAt`). The store accepts the same shapes the main process sends. AboutTab + UserPanel + AppLayout all read from the store, so no type drift between them.

**Placeholder scan:** no TODO/TBD/"handle appropriately" markers. All steps either show the exact code or describe a precise verification action.
