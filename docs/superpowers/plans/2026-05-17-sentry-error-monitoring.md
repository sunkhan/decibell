# Sentry Error Monitoring Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire `@sentry/electron` into the Decibell client so unhandled errors in renderer + main process land in a Sentry SaaS project, with readable production stack traces via source-map upload and a privacy-respecting opt-out.

**Architecture:** Two independent `Sentry.init()` calls (main + renderer) sharing one DSN baked into `resources/sentry.json` at CI time. Opt-out + anonymous install ID persist in the existing `AppSettings` blob; main process generates the ID on first boot and forwards it to the renderer via `webPreferences.additionalArguments` (same channel the media-server port uses). First-launch disclosure banner in `MainLayout`; permanent toggle in `PrivacyTab`. Source-map upload to Sentry's artifact store on tagged Linux CI builds.

**Tech Stack:** `@sentry/electron` v5, `@sentry/cli` (devDep), Vite (source-map emit), electron-builder (DSN packaging via `extraResources`), Rust + napi-rs (`AppSettings` schema), Zustand (renderer store), GitHub Actions (CI patches).

**Spec:** `docs/superpowers/specs/2026-05-17-sentry-error-monitoring-design.md` (commit `00dc00d`).

---

## Preconditions (user-side, before any task is useful)

These belong to `sunkhan`, not the implementation. The plan tasks should still all land without these in place — CI gates the Sentry-specific steps on `env.SENTRY_DSN != ''`, so missing secrets just mean builds skip Sentry without breaking:

1. Sentry project already created on sentry.io (done).
2. Project's "Strip IP" privacy setting toggled on (Settings → Security & Privacy → Prevent Storing of IP Addresses).
3. Four GitHub repository secrets set on `sunkhan/decibell`:
   - `SENTRY_DSN` — the project DSN, copied from sentry.io project settings.
   - `SENTRY_AUTH_TOKEN` — auth token with `project:read` + `project:releases` scope.
   - `SENTRY_ORG` — Sentry org slug.
   - `SENTRY_PROJECT` — Sentry project slug.

---

## File-level map

**New files:**

| Path | Responsibility |
|---|---|
| `electron-client/electron/main/sentry.ts` | `initMainSentry()` — owns all main-process Sentry interaction: DSN load, init guards, beforeSend, initial scope. |
| `electron-client/src/lib/sentry.ts` | `initRendererSentry()` — same shape for the renderer; reads boot config from preload bridge. |

**Modified files:**

| Path | Change |
|---|---|
| `electron-client/package.json` | Add `@sentry/electron` dep, `@sentry/cli` devDep. |
| `electron-client/native/src/config.rs` | Three new `AppSettings` fields: `crash_reporting_enabled` (default true), `crash_reporting_install_id` (Option), `crash_reporting_consent_shown`. |
| `electron-client/src/features/settings/loadSettings.ts` | Extend `LoadedConfigShape` with the three new fields; hydrate into store. |
| `electron-client/src/stores/uiStore.ts` | `crashReportingEnabled` + `crashReportingConsentShown` fields + setters. |
| `electron-client/src/features/settings/saveSettings.ts` | Serialize the three new fields. |
| `electron-client/electron/main/index.ts` | Load settings, generate install ID if missing, call `initMainSentry`, pass three args via `webPreferences.additionalArguments`. |
| `electron-client/electron/preload/index.ts` | Parse the three new args, expose `window.decibell.sentryConfig`. |
| `electron-client/src/types/global.d.ts` | Type `window.decibell.sentryConfig`. |
| `electron-client/src/main.tsx` | Call `initRendererSentry()` before `ReactDOM.createRoot`. |
| `electron-client/src/features/settings/tabs/PrivacyTab.tsx` | Append crash-reporting toggle below `friendsOnlyDms`. |
| `electron-client/src/layouts/MainLayout.tsx` | Mount first-launch disclosure banner. |
| `electron-client/src/layouts/AppLayout.tsx` | Subscribe to `chatStore.connectedServers`, set `Sentry.setTag("connected_servers", ...)`. |
| `electron-client/vite.config.ts` | `build.sourcemap: true`. |
| `electron-client/electron-builder.yml` | Add `resources/sentry.json` to `extraResources`; exclude `dist/renderer/assets/*.map` from `files`. |
| `electron-client/.gitignore` | Ignore `resources/sentry.json`. |
| `.github/workflows/electron-release.yml` | DSN bake step + source-map upload step (gated on tag + Linux runner). |

---

## Task list

### Task 1: Add the three `AppSettings` fields (Rust)

**Files:**
- Modify: `electron-client/native/src/config.rs:21-96`

- [ ] **Step 1: Add fields to the `AppSettings` struct**

Open `electron-client/native/src/config.rs`. Find the closing `}` of the `AppSettings` struct (line 96, right after `pub stream_enforced_codec: Option<u8>,`).

Insert these three fields immediately before the closing `}`:

```rust
    /// Crash reporting (Sentry) opt-out. Defaults to `true` (opt-out
    /// posture). Existing 0.6.5 configs without this field deserialize
    /// to the default via #[serde(default = "default_true")], so users
    /// upgrading get auto-opted-in.
    #[serde(default = "default_true")]
    pub crash_reporting_enabled: bool,
    /// Anonymous per-install identifier (UUID v4). Generated once on
    /// first boot of a Sentry-enabled build and persisted forever.
    /// Lets Sentry group multiple crashes from the same install without
    /// identifying the user.
    #[serde(default)]
    pub crash_reporting_install_id: Option<String>,
    /// Whether the first-launch disclosure banner has been shown.
    /// Flips true on dismiss; never reverted. Independent of `_enabled`
    /// (have we told the user? vs. is it on?).
    #[serde(default)]
    pub crash_reporting_consent_shown: bool,
```

`default_true` already exists at line 98-100 of this file — no new helper needed.

- [ ] **Step 2: Rebuild the native addon**

From `electron-client/`:

```sh
npm run build:native
```

Expected: `cargo build` succeeds; new `index.<platform>-<arch>-<libc>.node` written to `electron-client/native/`. No errors.

- [ ] **Step 3: Commit**

```sh
git add electron-client/native/src/config.rs
git commit -m "feat(config): three new AppSettings fields for crash reporting

crash_reporting_enabled defaults to true via serde, so existing 0.6.5
installs upgrade auto-opted-in. crash_reporting_install_id is None
until the main process generates a UUID on first boot. consent_shown
gates the one-time disclosure banner."
```

---

### Task 2: Store fields + setters in `uiStore`

**Files:**
- Modify: `electron-client/src/stores/uiStore.ts`

**Why all three flags live in the renderer store, not just two:** `save_settings` (the native command) takes the **full** `AppSettings` blob; any omitted field gets deserialized to its serde default and written back to disk, **clobbering the persisted value**. So the renderer must echo the install ID back through every save even though it never generates it (main does that on first boot, then pushes the ID into the renderer via `additionalArguments`).

- [ ] **Step 1: Add three fields + three setters to the interface**

Open `electron-client/src/stores/uiStore.ts`. Locate the state interface (typically `UiState`). Add:

```ts
crashReportingEnabled: boolean;
crashReportingConsentShown: boolean;
crashReportingInstallId: string | null;
setCrashReportingEnabled: (v: boolean) => void;
setCrashReportingConsentShown: (v: boolean) => void;
setCrashReportingInstallId: (v: string | null) => void;
```

- [ ] **Step 2: Add the three initial values + setters to the store creator**

Inside the `create((set) => ({ ... }))` block, add:

```ts
crashReportingEnabled: true,
crashReportingConsentShown: false,
crashReportingInstallId: null,
setCrashReportingEnabled: (v) => set({ crashReportingEnabled: v }),
setCrashReportingConsentShown: (v) => set({ crashReportingConsentShown: v }),
setCrashReportingInstallId: (v) => set({ crashReportingInstallId: v }),
```

- [ ] **Step 3: Verify renderer typecheck**

```sh
npx tsc -p tsconfig.web.json --noEmit
```

Expected: no NEW errors.

- [ ] **Step 4: Commit**

```sh
git add electron-client/src/stores/uiStore.ts
git commit -m "feat(settings): uiStore fields for crash-reporting state

Three boolean/null fields with setters. installId lives in the store
even though only main process generates it — saveSettings must
round-trip it back to disk to avoid clobbering the persisted UUID
when other settings change."
```

---

### Task 3: Hydrate from `load_config` + serialize via `save_settings`

**Files:**
- Modify: `electron-client/src/features/settings/loadSettings.ts`
- Modify: `electron-client/src/features/settings/saveSettings.ts`

- [ ] **Step 1: Add the three fields to `LoadedConfigShape`**

In `electron-client/src/features/settings/loadSettings.ts`, find the `settings: { ... }` block of `LoadedConfigShape` (lines 20-49). Append three lines immediately before the closing `}` of `settings: { ... }`, just after `stream_enforced_codec: number | null;`:

```ts
    crash_reporting_enabled: boolean;
    crash_reporting_install_id: string | null;
    crash_reporting_consent_shown: boolean;
```

- [ ] **Step 2: Hydrate the three store fields in `loadSettings.ts`**

In the same file, find the `// Privacy` block (around line 63-64):

```ts
  // Privacy
  useDmStore.getState().setFriendsOnlyDms(settings.friends_only_dms);
```

Append directly after:

```ts
  // Crash reporting (Sentry). Always hydrate all three — defaults
  // baked into the Rust schema cover users upgrading from a config
  // file that predates these fields.
  useUiStore.getState().setCrashReportingEnabled(settings.crash_reporting_enabled);
  useUiStore.getState().setCrashReportingInstallId(settings.crash_reporting_install_id);
  useUiStore.getState().setCrashReportingConsentShown(settings.crash_reporting_consent_shown);
```

- [ ] **Step 3: Serialize the three fields in `saveSettings.ts`**

Open `electron-client/src/features/settings/saveSettings.ts`. Inside the `invoke("save_settings", { ... })` call, append three keys at the end of the object (just before the closing `}` that precedes `.catch(...)`):

```ts
    crash_reporting_enabled: ui.crashReportingEnabled,
    crash_reporting_install_id: ui.crashReportingInstallId,
    crash_reporting_consent_shown: ui.crashReportingConsentShown,
```

- [ ] **Step 4: Verify renderer typecheck**

```sh
npx tsc -p tsconfig.web.json --noEmit
```

Expected: no NEW errors.

- [ ] **Step 5: Commit**

```sh
git add electron-client/src/features/settings/loadSettings.ts electron-client/src/features/settings/saveSettings.ts
git commit -m "feat(settings): round-trip crash-reporting fields through native config

loadSettings hydrates the three store fields from the load_config
response; saveSettings re-serializes them on every persist. install
ID is echoed back even though main is the only writer — required to
prevent the on-disk UUID from being wiped when other settings change."
```

---

### Task 4: Install dependencies

**Files:**
- Modify: `electron-client/package.json`

- [ ] **Step 1: Install runtime + CI dependencies**

From `electron-client/`:

```sh
npm install @sentry/electron@^5.0.0
npm install --save-dev @sentry/cli@^2.0.0
```

Expected: both added to `package.json`, lockfile updated. `@sentry/electron` lands in `dependencies` (must be in the asar); `@sentry/cli` lands in `devDependencies` (CI-only).

- [ ] **Step 2: Verify TS finds the types**

```sh
npx tsc -p tsconfig.node.json --noEmit
```

Expected: no errors. (TypeScript should be able to find `@sentry/electron/main` and `@sentry/electron/renderer` subpath exports.)

- [ ] **Step 3: Commit**

```sh
git add electron-client/package.json electron-client/package-lock.json
git commit -m "build(deps): add @sentry/electron + @sentry/cli

@sentry/electron is a regular dep (ships in the asar). @sentry/cli is
devDep (CI-only, runs from the GitHub Actions workspace to upload
source maps)."
```

---

### Task 5: Main-process Sentry module

**Files:**
- Create: `electron-client/electron/main/sentry.ts`

- [ ] **Step 1: Write the module**

Create `electron-client/electron/main/sentry.ts`:

```ts
import * as Sentry from "@sentry/electron/main";
import { app } from "electron";
import * as fs from "node:fs";
import * as path from "node:path";

// Owns all main-process Sentry interaction. Two gates protect against
// firing when we shouldn't:
//   - !app.isPackaged → dev mode, skip
//   - !args.enabled   → user opt-out, skip
//   - DSN missing     → no CI secret baked, skip
//
// Failure modes all collapse to "no events sent, one console.log".
// Sentry's own retry-with-disk-queue handles network blips after init.

interface InitArgs {
  enabled: boolean;
  installId: string;
}

interface DsnJson {
  dsn?: string;
}

function loadDsn(): string | null {
  if (!app.isPackaged) return null;
  try {
    const p = path.join(process.resourcesPath, "sentry.json");
    const j = JSON.parse(fs.readFileSync(p, "utf8")) as DsnJson;
    return j.dsn && j.dsn.length > 0 ? j.dsn : null;
  } catch {
    return null;
  }
}

export function initMainSentry(args: InitArgs): boolean {
  if (!app.isPackaged) {
    console.log("[sentry] disabled (dev mode)");
    return false;
  }
  if (!args.enabled) {
    console.log("[sentry] disabled (user opt-out)");
    return false;
  }
  const dsn = loadDsn();
  if (!dsn) {
    console.log("[sentry] disabled (no DSN baked)");
    return false;
  }

  Sentry.init({
    dsn,
    release: `decibell@${app.getVersion()}`,
    environment: "production",
    sampleRate: 1.0,
    autoSessionTracking: false,
    initialScope: {
      user: { id: args.installId },
      tags: {
        platform: process.platform,
        arch: process.arch,
        os_release: process.getSystemVersion?.() ?? "unknown",
        is_packaged: true,
      },
    },
    beforeSend(event) {
      // Belt-and-suspenders even though Sentry's project-side "Strip
      // IP" setting will also be enabled. We never want IPs in events.
      if (event.user) delete event.user.ip_address;
      return event;
    },
  });
  console.log(`[sentry] initialized (release=decibell@${app.getVersion()})`);
  return true;
}
```

- [ ] **Step 2: Verify the main-side TS build**

From `electron-client/`:

```sh
npm run build:tsc
```

Expected: no errors. Compiles `sentry.ts` to `dist/electron/main/sentry.js`.

- [ ] **Step 3: Commit**

```sh
git add electron-client/electron/main/sentry.ts
git commit -m "feat(sentry): initMainSentry — gated init for the main process

Single owner of @sentry/electron/main interaction. Three independent
guards (isPackaged, enabled, DSN present) collapse to a one-line
log on any miss; init only runs when all three pass. beforeSend
strips IP unconditionally as a belt-and-suspenders measure on top
of Sentry's project-level Strip-IP setting."
```

---

### Task 6: Wire main process boot

**Files:**
- Modify: `electron-client/electron/main/index.ts`

- [ ] **Step 1: Add imports**

Open `electron-client/electron/main/index.ts`. Find the existing import block (top of file). Add:

```ts
import * as crypto from "node:crypto";
import { callCommand } from "./addon";
import { initMainSentry } from "./sentry";
```

(`callCommand` may already be imported indirectly via `./ipc`; if so, the second import is harmless — TS deduplicates.)

- [ ] **Step 2: Add the boot-time settings load + install-ID generation helper**

Above the `app.whenReady().then(async () => { ... })` block (i.e., before line 450 in the current file), add a new helper function:

```ts
interface LoadedConfigForSentry {
  settings: {
    crash_reporting_enabled?: boolean;
    crash_reporting_install_id?: string | null;
    [k: string]: unknown;
  };
}

// Reads the persisted AppSettings, generates a UUID install ID if
// missing, and writes it back. Fire-and-forget on the save: if it
// fails we keep using the in-memory ID for this session and try
// again on the next boot. The save_settings command requires the
// full settings blob (any omitted field gets reset to its serde
// default on disk), so we spread the loaded settings rather than
// passing a partial.
async function loadSentryBootConfig(): Promise<{ enabled: boolean; installId: string }> {
  try {
    const config = (await callCommand("loadConfig", {})) as LoadedConfigForSentry;
    const enabled = config.settings.crash_reporting_enabled !== false;
    let installId = config.settings.crash_reporting_install_id ?? null;
    if (!installId) {
      installId = crypto.randomUUID();
      // Write back the full settings blob with the new ID merged in.
      const updatedSettings = {
        ...config.settings,
        crash_reporting_install_id: installId,
      };
      void callCommand("saveSettings", updatedSettings);
    }
    return { enabled, installId };
  } catch (e) {
    console.warn("[sentry] loadConfig failed; using ephemeral ID:", e);
    return { enabled: true, installId: crypto.randomUUID() };
  }
}
```

- [ ] **Step 3: Call it in the boot chain + init Sentry**

Find the `app.whenReady().then(async () => { ... })` block. Inside, after `initAddon();` (around line 543 in the current file, where the existing `initUpdater();` is), add:

```ts
  // Boot Sentry as early as possible, but after initAddon() (which
  // loads the napi addon — required for the loadConfig command we
  // call below). Failure here doesn't block app boot; the helper
  // returns sensible defaults on any error.
  const sentryConfig = await loadSentryBootConfig();
  initMainSentry({
    enabled: sentryConfig.enabled,
    installId: sentryConfig.installId,
  });
```

The order matters: `initAddon()` must run before `loadSentryBootConfig()` since the latter calls a napi command.

- [ ] **Step 4: Pass install ID + version + enabled flag via additionalArguments**

Find the `createWindow()` function (around line 252) and its `webPreferences: { additionalArguments: [...] }` block (around line 290-293).

Currently:

```ts
      additionalArguments: [
        `--decibell-media-server-port=${getMediaServerPort()}`,
      ],
```

`createWindow` is called from inside `whenReady` but currently takes no arguments. We need it to know the Sentry config. Two options: pass as arg, or use a module-level variable. Module-level is cleaner here since `loadSentryBootConfig` ran already.

Add a module-level holder right after the `let mainWindow: BrowserWindow | null = null;` line (around line 250):

```ts
let cachedSentryBoot: { enabled: boolean; installId: string } = {
  enabled: true,
  installId: "",
};
```

In the `whenReady` block, after `loadSentryBootConfig()`, store it:

```ts
  cachedSentryBoot = sentryConfig;
```

Then update the `additionalArguments` block:

```ts
      additionalArguments: [
        `--decibell-media-server-port=${getMediaServerPort()}`,
        `--decibell-sentry-enabled=${cachedSentryBoot.enabled ? "1" : "0"}`,
        `--decibell-install-id=${cachedSentryBoot.installId}`,
        `--decibell-version=${app.getVersion()}`,
      ],
```

- [ ] **Step 5: Verify main TS build**

```sh
npm run build:tsc
```

Expected: no errors.

- [ ] **Step 6: Commit**

```sh
git add electron-client/electron/main/index.ts
git commit -m "feat(sentry): wire initMainSentry into app boot + plumb config to renderer

loadSentryBootConfig runs once after initAddon (which loads the
napi bindings the helper needs). It generates a UUID install ID on
first boot and writes it back via save_settings. Three new
additionalArguments carry enabled/installId/version to the
renderer; preload bridge parses them in the next commit."
```

---

### Task 7: Preload bridge — expose `sentryConfig`

**Files:**
- Modify: `electron-client/electron/preload/index.ts`
- Modify: `electron-client/src/types/global.d.ts`

- [ ] **Step 1: Parse the three new args in preload**

Open `electron-client/electron/preload/index.ts`. Find the existing `mediaPortArg` parsing block (around line 74-79):

```ts
const mediaPortArg = process.argv.find((a) =>
  a.startsWith("--decibell-media-server-port="),
);
const mediaServerPort = mediaPortArg
  ? parseInt(mediaPortArg.split("=")[1], 10) || 0
  : 0;
```

Append immediately after it:

```ts
const sentryEnabledArg = process.argv.find((a) =>
  a.startsWith("--decibell-sentry-enabled="),
);
const installIdArg = process.argv.find((a) =>
  a.startsWith("--decibell-install-id="),
);
const versionArg = process.argv.find((a) =>
  a.startsWith("--decibell-version="),
);
const sentryConfig = {
  enabled: sentryEnabledArg === "--decibell-sentry-enabled=1",
  installId: installIdArg ? installIdArg.split("=")[1] : "",
  version: versionArg ? versionArg.split("=")[1] : "unknown",
};
```

- [ ] **Step 2: Expose `sentryConfig` on the bridge**

In the same file, find the `contextBridge.exposeInMainWorld("decibell", { ... })` block. The first few fields are `platform`, `mediaServerPort`, `invoke`, `listen`. After `mediaServerPort` (around line 91), add:

```ts
  sentryConfig,
```

- [ ] **Step 3: Mirror the type in `global.d.ts`**

Open `electron-client/src/types/global.d.ts`. Find the `mediaServerPort: number;` line (around line 8). After it, add:

```ts
      sentryConfig: {
        enabled: boolean;
        installId: string;
        version: string;
      };
```

- [ ] **Step 4: Verify both TS projects**

```sh
npm run typecheck
```

Expected: no new errors.

- [ ] **Step 5: Commit**

```sh
git add electron-client/electron/preload/index.ts electron-client/src/types/global.d.ts
git commit -m "feat(sentry): preload bridge exposes window.decibell.sentryConfig

Parses three additionalArguments from main and surfaces them
synchronously on the bridge. The renderer init reads them BEFORE
React mounts, so any throw during the first render lands in Sentry
instead of disappearing."
```

---

### Task 8: Renderer Sentry module + boot wiring

**Files:**
- Create: `electron-client/src/lib/sentry.ts`
- Modify: `electron-client/src/main.tsx`

- [ ] **Step 1: Write the renderer module**

Create `electron-client/src/lib/sentry.ts`:

```ts
import * as Sentry from "@sentry/electron/renderer";

// Initialize Sentry in the renderer. Reads boot config from the
// preload bridge — main process has already decided whether Sentry
// is enabled, generated the install ID, and passed both via
// additionalArguments. The renderer never makes that decision itself,
// so this function just consumes.
//
// DSN is inherited from main via @sentry/electron's internal IPC
// channel — no DSN appears in the renderer bundle, which means
// source maps stay clean.
export function initRendererSentry(): boolean {
  const config = window.decibell.sentryConfig;
  if (!config.enabled) {
    console.log("[sentry] renderer disabled");
    return false;
  }

  Sentry.init({
    release: `decibell@${config.version}`,
    environment: "production",
    sampleRate: 1.0,
    autoSessionTracking: false,
    initialScope: {
      user: { id: config.installId },
      tags: {
        platform: window.decibell.platform,
        is_packaged: true,
      },
    },
    beforeSend(event) {
      if (event.user) delete event.user.ip_address;
      return event;
    },
  });
  console.log(`[sentry] renderer initialized (release=decibell@${config.version})`);
  return true;
}
```

- [ ] **Step 2: Call it from `main.tsx` before React mounts**

Open `electron-client/src/main.tsx`. Find the existing import block (top of file). Add:

```ts
import { initRendererSentry } from "./lib/sentry";
```

Then find the line `loadSettings().catch(...)` (around line 22). **Before** this line — and importantly before `ReactDOM.createRoot(...)` — call init:

```ts
// Initialize Sentry FIRST, before any other boot work. Any throw
// inside loadSettings, probeDecoders, the React mount itself, etc.,
// is then captured by the SDK. Boot order matters: Sentry init must
// be the first executable statement that runs.
initRendererSentry();
```

So the new order at the top of `main.tsx`:

```ts
// ... imports ...

initRendererSentry();    // ← new, FIRST

window.addEventListener("beforeunload", () => {
  flushSaveSettings();
});

loadSettings().catch((e) =>
  console.warn("[boot] loadSettings failed:", e),
);
// ... rest ...
```

- [ ] **Step 3: Verify renderer build**

From `electron-client/`:

```sh
npm run build:renderer
```

Expected: build succeeds.

- [ ] **Step 4: Verify both TS projects**

```sh
npm run typecheck
```

Expected: no new errors.

- [ ] **Step 5: Commit**

```sh
git add electron-client/src/lib/sentry.ts electron-client/src/main.tsx
git commit -m "feat(sentry): renderer init before ReactDOM.createRoot

initRendererSentry runs as the first executable statement in
main.tsx so any throw during the React mount lands in Sentry.
DSN auto-inherits from main via @sentry/electron's IPC; no DSN
appears in the renderer bundle (keeps source-map uploads clean)."
```

---

### Task 9: PrivacyTab toggle

**Files:**
- Modify: `electron-client/src/features/settings/tabs/PrivacyTab.tsx`

- [ ] **Step 1: Append the crash-reporting toggle**

Replace the entire contents of `electron-client/src/features/settings/tabs/PrivacyTab.tsx` with:

```tsx
import { invoke } from "../../../lib/ipc";
import { useDmStore } from "../../../stores/dmStore";
import { useUiStore } from "../../../stores/uiStore";
import { saveSettings } from "../saveSettings";

export default function PrivacyTab() {
  const friendsOnlyDms = useDmStore((s) => s.friendsOnlyDms);
  const crashReportingEnabled = useUiStore((s) => s.crashReportingEnabled);

  const handleToggleDms = () => {
    const newValue = !friendsOnlyDms;
    useDmStore.getState().setFriendsOnlyDms(newValue);
    invoke("set_dm_privacy", { friendsOnly: newValue }).catch(console.error);
    saveSettings();
  };

  const handleToggleCrashReporting = () => {
    const next = !crashReportingEnabled;
    useUiStore.getState().setCrashReportingEnabled(next);
    saveSettings();
    // No live SDK teardown — takes effect on next launch.
  };

  return (
    <div className="flex flex-col gap-3">
      <ToggleRow
        title="Only accept DMs from friends"
        description="When enabled, only users in your friends list can send you direct messages"
        value={friendsOnlyDms}
        onToggle={handleToggleDms}
      />
      <ToggleRow
        title="Send anonymous crash reports"
        description="Helps fix bugs that happen in the field. No usernames, no message contents, no server names. Restart required for changes to apply."
        value={crashReportingEnabled}
        onToggle={handleToggleCrashReporting}
      />
    </div>
  );
}

function ToggleRow({
  title,
  description,
  value,
  onToggle,
}: {
  title: string;
  description: string;
  value: boolean;
  onToggle: () => void;
}) {
  return (
    <div className="flex items-center justify-between rounded-[10px] border border-border-divider bg-bg-light px-4 py-3.5 transition-colors hover:bg-bg-lighter">
      <div className="pr-4">
        <div className="text-[14px] font-medium text-text-primary">{title}</div>
        <div className="mt-1 text-[12px] leading-relaxed text-text-muted">
          {description}
        </div>
      </div>
      <button
        onClick={onToggle}
        className={`relative h-[22px] w-[40px] shrink-0 rounded-full border transition-all ${
          value
            ? "border-accent bg-accent shadow-[0_0_8px_rgba(56,143,255,0.22)]"
            : "border-border bg-bg-lighter"
        }`}
      >
        <div
          className={`absolute top-[3px] h-[16px] w-[16px] rounded-full transition-all ${
            value ? "translate-x-[18px] bg-white" : "translate-x-[3px] bg-text-muted"
          }`}
        />
      </button>
    </div>
  );
}
```

The inline `ToggleRow` component absorbs the duplicated markup from the two rows — clean for a third row when we add usage analytics later.

- [ ] **Step 2: Verify renderer build**

```sh
npm run build:renderer
```

Expected: build succeeds.

- [ ] **Step 3: Commit**

```sh
git add electron-client/src/features/settings/tabs/PrivacyTab.tsx
git commit -m "feat(privacy): crash-reporting toggle in Privacy tab

Second row in Privacy settings. Restart-required copy is the
design lever — we skip live Sentry teardown by telling the user
to relaunch. ToggleRow inline component pulls the shared visual
out of the two rows so a third (analytics, later) drops in clean."
```

---

### Task 10: First-launch disclosure banner

**Files:**
- Create: `electron-client/src/components/CrashReportingBanner.tsx`
- Modify: `electron-client/src/layouts/MainLayout.tsx`

- [ ] **Step 1: Create the banner component**

Create `electron-client/src/components/CrashReportingBanner.tsx`:

```tsx
import { useUiStore } from "../stores/uiStore";
import { saveSettings } from "../features/settings/saveSettings";

// First-launch disclosure for crash reporting. Mounted in MainLayout
// (post-login chrome). Shown until the user dismisses; never reappears
// after that. No "decline" button — opt-out lives in Settings →
// Privacy. The toggle in PrivacyTab can revert the user's choice;
// this banner is a one-way disclosure.
export default function CrashReportingBanner() {
  const shown = useUiStore((s) => s.crashReportingConsentShown);
  if (shown) return null;

  const dismiss = () => {
    useUiStore.getState().setCrashReportingConsentShown(true);
    saveSettings();
  };

  return (
    <div className="flex h-9 shrink-0 items-center justify-between border-b border-border-divider bg-accent-soft px-4 text-[12px] text-accent-bright">
      <span>
        Decibell sends anonymous crash reports to help us fix bugs. You can
        disable this in Settings → Privacy.
      </span>
      <button
        onClick={dismiss}
        className="ml-3 flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-accent-bright transition-colors hover:bg-accent-mid"
        title="Dismiss"
      >
        <svg
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.5"
          strokeLinecap="round"
        >
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </svg>
      </button>
    </div>
  );
}
```

- [ ] **Step 2: Mount it in MainLayout**

Open `electron-client/src/layouts/MainLayout.tsx`. Find the existing imports and add:

```ts
import CrashReportingBanner from "../components/CrashReportingBanner";
```

Find the return statement (around line 71-72):

```tsx
  return (
    <div className="flex h-full w-full flex-col">
      {connectionStatus === "reconnecting" && (
        <div className="flex h-8 shrink-0 items-center justify-center bg-warning text-xs font-semibold text-bg-primary">
          Connection lost. Reconnecting...
        </div>
      )}

      <ServerBar />
```

Replace with:

```tsx
  return (
    <div className="flex h-full w-full flex-col">
      {connectionStatus === "reconnecting" && (
        <div className="flex h-8 shrink-0 items-center justify-center bg-warning text-xs font-semibold text-bg-primary">
          Connection lost. Reconnecting...
        </div>
      )}

      <CrashReportingBanner />

      <ServerBar />
```

Banner sits below the reconnecting bar (if visible) and above the ServerBar. If both render, they stack — reconnecting is rare-and-temporary so this is the right order.

- [ ] **Step 3: Verify renderer build**

```sh
npm run build:renderer
```

Expected: build succeeds.

- [ ] **Step 4: Commit**

```sh
git add electron-client/src/components/CrashReportingBanner.tsx electron-client/src/layouts/MainLayout.tsx
git commit -m "feat(privacy): first-launch crash-reporting disclosure banner

Mounted in MainLayout (not AppLayout) so the login screen doesn't
show it. accent-soft styling keeps it informational rather than
alarming. Dismiss flips crashReportingConsentShown + saveSettings;
banner never reappears after that. Opt-out lives in PrivacyTab —
this banner doesn't ask, it tells."
```

---

### Task 11: `connected_servers` tag

**Files:**
- Modify: `electron-client/src/layouts/AppLayout.tsx`

- [ ] **Step 1: Subscribe to chatStore.connectedServers and push tag**

Open `electron-client/src/layouts/AppLayout.tsx`. Find the existing imports and add:

```ts
import * as Sentry from "@sentry/electron/renderer";
import { useChatStore } from "../stores/chatStore";
```

Inside the `AppLayout` component, find the existing `useEffect` that mounts the update_status listener (around lines 24-50 from the auto-update work). After that `useEffect`, add a second one:

```ts
  // Track how many community servers this install is connected to.
  // Helps reproduce "happens when N+ servers connected" bug reports.
  // Sentry.setTag is a no-op when the SDK isn't initialized, so this
  // runs unconditionally — the gate lives in initRendererSentry.
  useEffect(() => {
    const apply = (size: number) => {
      Sentry.setTag("connected_servers", String(size));
    };
    apply(useChatStore.getState().connectedServers.size);
    return useChatStore.subscribe((state) => {
      apply(state.connectedServers.size);
    });
  }, []);
```

- [ ] **Step 2: Verify renderer build**

```sh
npm run build:renderer
```

Expected: build succeeds.

- [ ] **Step 3: Commit**

```sh
git add electron-client/src/layouts/AppLayout.tsx
git commit -m "feat(sentry): connected_servers tag, kept in sync via chatStore subscribe

AppLayout subscribes once at mount and pushes Sentry.setTag on
every change. Helps correlate 'happens with many servers' reports
to actual server counts. Sentry.setTag is safe to call when the
SDK isn't initialized (it's a scope mutation, not a network call),
so we don't need to gate it on initRendererSentry's return value."
```

---

### Task 12: Vite source maps + electron-builder packaging + .gitignore

**Files:**
- Modify: `electron-client/vite.config.ts`
- Modify: `electron-client/electron-builder.yml`
- Modify: `electron-client/.gitignore`

- [ ] **Step 1: Enable source maps in Vite production build**

Open `electron-client/vite.config.ts`. Find the `build: { ... }` block (around lines 14-19):

```ts
  build: {
    outDir: "dist/renderer",
    emptyOutDir: true,
    target: "chrome120",
  },
```

Replace with:

```ts
  build: {
    outDir: "dist/renderer",
    emptyOutDir: true,
    target: "chrome120",
    // Emit .js.map files so sentry-cli can upload them on tagged
    // releases. The maps are excluded from the packaged build via
    // electron-builder.yml so they reach Sentry's artifact store
    // but not the user's machine.
    sourcemap: true,
  },
```

- [ ] **Step 2: Add `sentry.json` to extraResources + exclude source maps from package**

Open `electron-client/electron-builder.yml`. Find the existing `files:` block and the existing `extraResources:` block.

For `extraResources:`, currently:

```yaml
extraResources:
  - from: resources/icon.png
    to: icon.png
```

Replace with:

```yaml
extraResources:
  - from: resources/icon.png
    to: icon.png
  # CI writes resources/sentry.json from the SENTRY_DSN secret before
  # packaging. Local builds without the secret leave the file absent;
  # main process's initMainSentry handles missing-DSN as "skip init".
  - from: resources/sentry.json
    to: sentry.json
    filter: ["**/*"]
```

For `files:`, currently the block starts with:

```yaml
files:
  - dist/**/*
  - native/index.js
```

Add an exclusion line right after `dist/**/*`:

```yaml
files:
  - dist/**/*
  - "!dist/renderer/assets/*.map"
  - native/index.js
```

- [ ] **Step 3: Ignore the local sentry.json**

Open `electron-client/.gitignore`. Add at the bottom (or alphabetically next to other resource entries):

```
# Sentry DSN baked by CI on release builds — never tracked.
resources/sentry.json
```

- [ ] **Step 4: Verify a local build still works**

From `electron-client/`:

```sh
npm run build:renderer
ls dist/renderer/assets/*.map | head -3
```

Expected: renderer build succeeds; `.map` files exist in `dist/renderer/assets/`.

- [ ] **Step 5: Commit**

```sh
git add electron-client/vite.config.ts electron-client/electron-builder.yml electron-client/.gitignore
git commit -m "build(sentry): source maps + DSN packaging plumbing

Vite emits .js.map files; electron-builder ships resources/sentry.json
into the packaged Resources folder (CI writes it from the secret;
local builds leave it absent and skip Sentry); .map files are
excluded from the asar so they reach Sentry but not users."
```

---

### Task 13: CI — DSN bake + source-map upload

**Files:**
- Modify: `.github/workflows/electron-release.yml`

- [ ] **Step 1: Insert DSN bake step**

Open `.github/workflows/electron-release.yml`. Find the existing "Package" step (around line 158):

```yaml
      - name: Package
        # Don't expose GH_TOKEN here. ...
        run: npx electron-builder --publish=never
```

Insert a new step immediately **before** the Package step:

```yaml
      - name: Bake Sentry DSN
        if: env.SENTRY_DSN != ''
        env:
          SENTRY_DSN: ${{ secrets.SENTRY_DSN }}
        shell: bash
        run: |
          mkdir -p electron-client/resources
          echo "{\"dsn\":\"$SENTRY_DSN\"}" > electron-client/resources/sentry.json
          echo "[sentry] DSN written to electron-client/resources/sentry.json"
```

- [ ] **Step 2: Insert source-map upload step**

Insert a second new step **after** the Package step:

```yaml
      - name: Upload source maps to Sentry
        if: matrix.target_label == 'linux' && startsWith(github.ref, 'refs/tags/ev') && env.SENTRY_AUTH_TOKEN != ''
        env:
          SENTRY_AUTH_TOKEN: ${{ secrets.SENTRY_AUTH_TOKEN }}
          SENTRY_ORG: ${{ secrets.SENTRY_ORG }}
          SENTRY_PROJECT: ${{ secrets.SENTRY_PROJECT }}
        shell: bash
        run: |
          VERSION="${GITHUB_REF#refs/tags/ev}"
          echo "[sentry] uploading source maps for decibell@${VERSION}"
          npx @sentry/cli sourcemaps inject \
            --release "decibell@${VERSION}" \
            electron-client/dist/renderer
          npx @sentry/cli sourcemaps upload \
            --release "decibell@${VERSION}" \
            electron-client/dist/renderer
```

The triple gate (Linux runner + tagged build + auth token present) ensures the upload runs exactly once per release and skips on forks / local CI experiments.

- [ ] **Step 3: Commit**

```sh
git add .github/workflows/electron-release.yml
git commit -m "ci(sentry): bake DSN into package + upload source maps on tagged releases

Bake step runs on every matrix runner before electron-builder so
the DSN ends up in the packaged Resources folder per platform.
Upload step runs once (Linux + tag + token present) and skips
silently otherwise — fork builds and local PRs see no Sentry
machinery activate."
```

---

### Task 14: Local packaged build verification (user drives)

This task is for `sunkhan` to run once after Tasks 1-13 are merged. The implementation is complete at that point; this is the smoke-test pass.

- [ ] **Step 1: Confirm the four GitHub secrets are set**

On `github.com/sunkhan/decibell/settings/secrets/actions`, verify:
- `SENTRY_DSN`
- `SENTRY_AUTH_TOKEN`
- `SENTRY_ORG`
- `SENTRY_PROJECT`

- [ ] **Step 2: Local build with the DSN injected manually**

From `electron-client/`:

```sh
mkdir -p resources
echo '{"dsn":"<paste-real-dsn-here>"}' > resources/sentry.json
npm run package
```

Expected: `release/Decibell-0.6.5-x64.exe` (or `.AppImage` on Linux) plus source-map files in `dist/renderer/assets/*.map`.

Delete `resources/sentry.json` after to avoid accidentally committing it:

```sh
rm resources/sentry.json
```

- [ ] **Step 3: Install + launch**

Run the packaged installer. On launch, watch the terminal / DevTools console for:

- `[sentry] initialized (release=decibell@0.6.5)` — main process
- `[sentry] renderer initialized (release=decibell@0.6.5)` — renderer
- The first-launch banner appears in MainLayout (post-login).

- [ ] **Step 4: Verify Privacy tab toggle**

Open Settings → Privacy. The "Send anonymous crash reports" toggle should be visible with the value ON. Toggle it; verify the banner copy mentions "Restart required". Don't actually toggle off for the verification pass.

- [ ] **Step 5: Trigger a deliberate test crash**

Open Settings → About. (We'll wire a hidden dev-only "throw test error" button in a follow-up if useful; for now, the simplest test is to open DevTools and run in the console:

```js
throw new Error("Sentry verification: renderer test crash");
```

Wait 5-10 seconds, then check the Sentry dashboard. A new issue titled "Sentry verification: renderer test crash" should appear, tagged with `platform=win32` (or your platform), `is_packaged=true`, `connected_servers=<your count>`, and a release of `decibell@0.6.5`.

- [ ] **Step 6: Verify source maps after the next release**

The current 0.6.5 release predates the source-map upload step. The verification of readable stack traces happens on the **next** release (whenever you push `ev0.6.6`): the test crash from that build should show the unminified source location (e.g. `ChatPanel.tsx:194`) in the Sentry frame instead of `index-XXXX.js:1:1428`.

If any step fails:
- No `[sentry] initialized` log + DSN looks right: open Sentry dashboard, check the project for connection rejections.
- Banner doesn't appear: check `useUiStore.getState().crashReportingConsentShown` in DevTools console — should be `false` on first launch.
- Issue arrives but stack trace is minified: source-map upload didn't run. Re-check the Linux runner's logs for the "Upload source maps" step.

---

## Self-review

**Spec coverage check:**

| Spec section | Task(s) |
|---|---|
| Goal / non-goals | Plan implements goal; non-goals deliberately untouched. |
| Architecture overview | Tasks 5-8 (main + preload + renderer plumbing). |
| Persistence — Rust schema | Task 1. |
| Persistence — install-ID flow | Task 6. |
| Persistence — renderer store + saveSettings | Tasks 2-3. |
| Main process Sentry init | Tasks 5-6. |
| Renderer Sentry init | Task 8. |
| Preload bridge + global.d.ts | Task 7. |
| Privacy + scrubbing | Tasks 5, 8 (`beforeSend` + `Strip IP` belt-and-suspenders). |
| CI — DSN bake | Task 13. |
| CI — source-map upload | Task 13. |
| UI — disclosure banner | Task 10. |
| UI — PrivacyTab toggle | Task 9. |
| `connected_servers` tag | Task 11. |
| Failure modes | Covered inline in `initMainSentry`, `loadSentryBootConfig` catch, CI step `if:` guards. |

**Type consistency check:** `crashReportingEnabled`, `crashReportingConsentShown`, `crashReportingInstallId` are all defined once in Task 2/3 and referenced consistently after. `LoadedConfigShape.settings.crash_reporting_*` keys match the Rust struct field names (snake_case via serde's default). `additionalArguments` arg names (`--decibell-sentry-enabled`, `--decibell-install-id`, `--decibell-version`) are parsed identically in Task 6 (main) and Task 7 (preload).

**Placeholder scan:** no TODO/TBD/"handle appropriately" markers. Task 2's preamble flags the "must echo install ID back through save_settings" subtlety so the executor doesn't accidentally omit it.
