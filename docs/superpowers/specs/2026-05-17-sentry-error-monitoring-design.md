# Sentry Error Monitoring Design

**Date:** 2026-05-17
**Status:** Approved
**Owner:** sunkhan

## Goal

Wire `@sentry/electron` into the Decibell client so unhandled errors in the
renderer and main process get captured to a Sentry SaaS project. First step
of a broader monitoring story — **errors only**, no usage analytics, no
performance traces, no session replay. Anonymous by default; user can opt
out via Settings → Privacy.

## Non-goals

- **Usage analytics.** Feature adoption, retention, daily active users — that's
  a separate brainstorm. Picking analytics tooling (PostHog vs. lighter
  alternatives) is its own design conversation.
- **Performance monitoring.** No `tracesSampleRate`, no transaction spans.
- **Session replay.** Not enabled; would require its own privacy review.
- **Native (Rust) error capture.** The napi-rs addon stays uncovered for v1.
  Most production failures we've seen sit in the renderer or main process;
  Rust panics propagate to JS as caught napi errors and land in the JS-side
  capture anyway.
- **User feedback widget.** No in-app "report a bug" UI; comments from users
  attach via the Sentry dashboard, not from inside Decibell.
- **Self-hosted Sentry.** Start with the SaaS free tier (5k errors/month).
  Migration to self-hosted is a DSN swap if it ever becomes a priority.
- **Live toggle.** Flipping the opt-out in Settings takes effect on next
  launch — the toggle's helper text says so.

## Architecture overview

```
┌─────────────────────────────────────────────────────────────┐
│  Main process                                                │
│  ─────────────                                              │
│  1. Load AppSettings (config.json on disk)                  │
│  2. Generate install_id if missing; persist via save_settings│
│  3. Read resources/sentry.json (CI-baked DSN, or empty)     │
│  4. initMainSentry({ enabled, installId, dsn, tags })       │
│  5. Pass enabled + installId via webPreferences.            │
│     additionalArguments to the new BrowserWindow            │
└────────────────────────────────┬────────────────────────────┘
                                 │ --decibell-sentry-enabled=1
                                 │ --decibell-install-id=<uuid>
                                 ▼
┌─────────────────────────────────────────────────────────────┐
│  Preload bridge (electron/preload/index.ts)                 │
│  - Parses the additionalArguments                            │
│  - Exposes window.decibell.sentryConfig as a synchronous     │
│    property (no IPC round-trip)                              │
└────────────────────────────────┬────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────┐
│  Renderer (src/main.tsx)                                    │
│  - initRendererSentry() runs BEFORE ReactDOM.createRoot     │
│  - Reads window.decibell.sentryConfig synchronously         │
│  - DSN is inherited from main via @sentry/electron's IPC    │
│  - Same release tag, same installId, additional             │
│    renderer-specific tags                                    │
└─────────────────────────────────────────────────────────────┘
```

**Two SDK inits, one source of truth.** Main owns the persisted opt-out and
install ID; both flow to the renderer via the existing
`webPreferences.additionalArguments` channel (the same one we use to
pass the media server port today). Renderer reads them synchronously from
the preload bridge — no IPC race where Sentry initializes after React
already crashed.

**Library choice:** `@sentry/electron` v5. One package, two subpath entries
(`/main`, `/renderer`). The renderer SDK auto-discovers the DSN from the
main process via Electron IPC, so the DSN never appears in the renderer
bundle — keeping source-map uploads clean.

**Bundle cost:** ~150 KB minified added to the renderer. Within budget for
the value (readable production crash reports).

## Persistence

### `native/src/config.rs` — three new fields on `AppSettings`

```rust
pub struct AppSettings {
    // ... existing fields ...

    /// User-facing opt-out for crash reporting. Defaults to true
    /// (opt-out posture decided 2026-05-17). Existing configs without
    /// this field deserialize with the default via serde, so users
    /// upgrading from 0.6.5 are auto-opted-in.
    #[serde(default = "default_true")]
    pub crash_reporting_enabled: bool,

    /// Anonymous per-install identifier. Generated once on first boot
    /// of a Sentry-enabled build (0.6.6+) and persisted forever.
    /// uuid::Uuid::new_v4().to_string().
    #[serde(default)]
    pub crash_reporting_install_id: Option<String>,

    /// Whether the first-launch disclosure banner has been shown.
    /// Flips true the first time the user dismisses the banner; never
    /// reverted. Independent of `_enabled` because they answer
    /// different questions (have we told the user? vs. is it on?).
    #[serde(default)]
    pub crash_reporting_consent_shown: bool,
}

fn default_true() -> bool { true }
```

### Install-ID generation flow

```text
main process boot
  ├─ load AppSettings
  ├─ if crash_reporting_install_id is None:
  │     install_id = uuid::Uuid::new_v4().to_string()
  │     mutate in-memory AppSettings
  │     enqueue async save_settings write-back
  ├─ if sentry.json present + dsn non-empty + crash_reporting_enabled:
  │     initMainSentry({ enabled: true, installId, dsn, tags })
  └─ createWindow() with additionalArguments carrying enabled + installId
```

### Renderer-side store

`useUiStore` gains:

```ts
crashReportingEnabled: boolean;            // default true
crashReportingConsentShown: boolean;       // default false
setCrashReportingEnabled: (v: boolean) => void;
setCrashReportingConsentShown: (v: boolean) => void;
```

Initial values hydrated from native's `request_settings` response (same
path used for every other persisted setting). `saveSettings.ts` adds the
three keys to its serialized blob. No new IPC.

## Main process

### `electron/main/sentry.ts` (new)

```ts
import * as Sentry from "@sentry/electron/main";
import { app } from "electron";
import * as fs from "node:fs";
import * as path from "node:path";

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
      // Belt-and-suspenders even though the project-side "Strip IP"
      // setting will also be enabled.
      if (event.user) delete event.user.ip_address;
      return event;
    },
  });
  console.log(`[sentry] initialized (release=decibell@${app.getVersion()})`);
  return true;
}
```

### `electron/main/index.ts` — wire it in

```ts
import { initMainSentry } from "./sentry";

// Inside app.whenReady().then(...) chain, BEFORE anything else can throw:
const settings = await loadSettings();
let installId = settings.crash_reporting_install_id;
if (!installId) {
  installId = crypto.randomUUID();
  settings.crash_reporting_install_id = installId;
  void persistSettings(settings); // fire-and-forget
}
initMainSentry({
  enabled: settings.crash_reporting_enabled,
  installId,
});

// Then, inside createWindow():
const sentryEnabled = settings.crash_reporting_enabled ? "1" : "0";
mainWindow = new BrowserWindow({
  // ...
  webPreferences: {
    // ...
    additionalArguments: [
      `--decibell-media-server-port=${getMediaServerPort()}`,
      `--decibell-sentry-enabled=${sentryEnabled}`,
      `--decibell-install-id=${installId}`,
      `--decibell-version=${app.getVersion()}`,
    ],
  },
});
```

## Renderer

### `electron/preload/index.ts` — expose `sentryConfig`

```ts
// At the top, alongside the existing mediaServerPort parsing:
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

// Inside contextBridge.exposeInMainWorld("decibell", { ... }):
sentryConfig,
```

Mirror the type in `src/types/global.d.ts`:

```ts
sentryConfig: {
  enabled: boolean;
  installId: string;
  version: string;
};
```

### `src/lib/sentry.ts` (new)

```ts
import * as Sentry from "@sentry/electron/renderer";

export function initRendererSentry(): boolean {
  const config = window.decibell.sentryConfig;
  if (!config.enabled) return false;

  Sentry.init({
    // DSN auto-inherited from main via @sentry/electron's internal IPC.
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
  return true;
}
```

Version, install ID, and the enabled flag all arrive through the same `additionalArguments` channel as the media server port — preload parses them synchronously, exposes via `window.decibell.sentryConfig`, no IPC round-trip or build-time substitution machinery needed.

### `src/main.tsx` — initialize before React mounts

```ts
import { initRendererSentry } from "./lib/sentry";
initRendererSentry();   // ← BEFORE ReactDOM.createRoot

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
```

The order matters: any throw during the initial mount lands in Sentry instead of disappearing into the developer console nobody is watching.

### `connected_servers` tag — added after first paint

Inside `useServerEvents.ts`'s `memberships_received` handler (which runs once on login and again on any roster change), after the store update:

```ts
Sentry.setTag("connected_servers", String(connectedServers.size));
```

Skipped when Sentry isn't initialized.

## Privacy + scrubbing

### Three layers of defense

| Layer | Concretely |
|---|---|
| **Init-time** | `autoSessionTracking: false`, no `tracesSampleRate`, no replay integration. `beforeSend` deletes `event.user.ip_address`. Sentry project "Strip IP" setting enabled in dashboard. |
| **Tag whitelist** | Only the tags listed in §3: `platform`, `arch`, `os_release`, `is_packaged`, `connected_servers`. No `username`, no email, no server/channel names, no message contents. User ID is the anon `install_id` UUID. |
| **Source-code hygiene** | Existing `console.error` call sites already follow the pattern `[domain] generic message: <err>` (we audited a sample). v1.1 task: full grep audit of `console.error`/`throw new Error` and rewrite any sites that embed user-controlled strings. |

### Defaults kept (because they're useful and PII-safe)

- Stack traces with our own filenames + line numbers — identifies our code, not the user.
- `console.error` / `console.warn` breadcrumbs — short text, no user data per the audit.
- Fetch / XHR breadcrumbs — URLs only; our URLs are `decibell-attachment://<serverId>/<attachmentId>`, both numeric, no PII.

### Defaults disabled

- Performance traces, session replay, user-feedback widgets.
- IP address auto-population.

## CI changes

Three new GitHub secrets:
- `SENTRY_DSN` — the project DSN (string, e.g. `https://abc@o123.ingest.sentry.io/456`).
- `SENTRY_AUTH_TOKEN` — auth token for source-map upload.
- `SENTRY_ORG` — Sentry org slug.
- `SENTRY_PROJECT` — Sentry project slug.

### Patch 1 — bake DSN into packaged app

New step in `.github/workflows/electron-release.yml`, runs on every matrix runner before `electron-builder`:

```yaml
- name: Bake Sentry DSN
  if: env.SENTRY_DSN != ''
  env:
    SENTRY_DSN: ${{ secrets.SENTRY_DSN }}
  shell: bash
  run: |
    echo "{\"dsn\":\"$SENTRY_DSN\"}" > electron-client/resources/sentry.json
```

`electron-builder.yml`'s `extraResources` block (which currently only
lists `icon.png`) gets a second entry:

```yaml
extraResources:
  - from: resources/icon.png
    to: icon.png
  - from: resources/sentry.json
    to: sentry.json
    # If the CI step didn't run (local build without secret), skip
    # silently rather than failing the package.
```

The file is gitignored so it never lives in the working tree.

### Patch 2 — upload source maps on tagged releases

```yaml
- name: Upload source maps to Sentry
  if: matrix.target_label == 'linux' && startsWith(github.ref, 'refs/tags/ev')
  env:
    SENTRY_AUTH_TOKEN: ${{ secrets.SENTRY_AUTH_TOKEN }}
    SENTRY_ORG: ${{ secrets.SENTRY_ORG }}
    SENTRY_PROJECT: ${{ secrets.SENTRY_PROJECT }}
  shell: bash
  run: |
    VERSION="${GITHUB_REF#refs/tags/ev}"
    npx @sentry/cli sourcemaps inject \
      --release "decibell@${VERSION}" \
      electron-client/dist/renderer
    npx @sentry/cli sourcemaps upload \
      --release "decibell@${VERSION}" \
      electron-client/dist/renderer
```

Gated to Linux runner + tagged builds so the upload runs exactly once per
release. `sourcemaps inject` rewrites the bundle to embed Sentry debug IDs;
`sourcemaps upload` ships the `.js.map` files to Sentry's artifact store.
Failure here doesn't block the build (sentry-cli treats network failures
as warnings); worst case is one release with minified stack traces in
Sentry until the next upload succeeds.

### Vite config — emit source maps in production

```ts
// vite.config.ts
build: {
  sourcemap: true,    // ← currently default-off in production
  // ...
}
```

Source maps are emitted to `dist/renderer/assets/*.js.map`. Vite places
them next to the bundled JS, which electron-builder would otherwise
pick up via `dist/**/*`. We add a glob to electron-builder's `files`
block to exclude them so they don't ship to users:

```yaml
# electron-builder.yml
files:
  - dist/**/*
  - "!dist/renderer/assets/*.map"
  # ... existing entries ...
```

Source maps reach Sentry's artifact store; users get a slim build.

## UI

### First-launch banner

Mounted in `MainLayout` (not `AppLayout`) — login screen doesn't show it.
Renders only while `crashReportingConsentShown === false`. Sits at the top
of `MainLayout`'s flex column, between any reconnecting bar and the
`ServerBar`. Styling: `bg-accent-soft text-accent-bright` with a
`border-b border-border-divider`. Inline copy:

> Decibell sends anonymous crash reports to help us fix bugs. You can
> disable this in Settings → Privacy. **✕**

Close button on the right. Click → `setCrashReportingConsentShown(true)`
+ `saveSettings()` → banner unmounts. No "decline" affordance — opt-out
lives in Settings.

### PrivacyTab toggle

Appended below the existing `friendsOnlyDms` toggle. Same visual primitive
(`relative h-[22px] w-[40px] rounded-full` slider). Copy:

> **Send anonymous crash reports**
> Helps fix bugs that happen in the field. No usernames, no message
> contents, no server names. Restart required for changes to apply.

Toggle handler:

```ts
const handleToggleCrashReporting = () => {
  const next = !crashReportingEnabled;
  useUiStore.getState().setCrashReportingEnabled(next);
  saveSettings();
};
```

The "Restart required" copy is the lever that lets us avoid live SDK
teardown/init. Users who flip privacy toggles are typically in a
restart-friendly mood anyway.

## Failure modes

| Failure | Behavior |
|---|---|
| `sentry.json` missing or empty | `initMainSentry()` returns `false`. One `console.log("[sentry] disabled (no DSN)")` at boot. No events sent. |
| Network unreachable when sending | Sentry SDK queues events to disk and retries on next launch. Built-in; no special handling. |
| DSN typo / invalid | First send fails with 4xx in SDK logs; no events arrive. Operator notices via empty Sentry dashboard after release. |
| `beforeSend` throws | Sentry catches its own callbacks internally; app keeps running. |
| User has Sentry blocked at firewall | Same as network unreachable: queued, eventually dropped. No user-visible impact. |
| Source-map upload fails in CI | Build still succeeds; stack traces minified for that release. Fix in next release. |
| User opts out mid-session | Setting persists immediately; takes effect next launch (per the UI copy). |
| Install-ID write-back fails | The ID stays only in memory for this session; regenerated next launch. Slight chance of duplicate "users" in Sentry until the write succeeds. Acceptable. |

## File-level changes

**New files:**
- `electron-client/electron/main/sentry.ts` (~80 LOC)
- `electron-client/src/lib/sentry.ts` (~40 LOC)

**Modified files:**
- `electron-client/package.json` — add `@sentry/electron` dep, `@sentry/cli` devDep
- `electron-client/native/src/config.rs` — three new `AppSettings` fields
- `electron-client/electron/main/index.ts` — call `initMainSentry()`, pass install ID via `additionalArguments`
- `electron-client/electron/preload/index.ts` — parse + expose `sentryConfig`
- `electron-client/src/types/global.d.ts` — type `window.decibell.sentryConfig`
- `electron-client/src/main.tsx` — call `initRendererSentry()` before React mounts
- `electron-client/src/stores/uiStore.ts` — `crashReportingEnabled`, `crashReportingConsentShown` fields + setters
- `electron-client/src/features/settings/saveSettings.ts` — serialize the three new fields
- `electron-client/src/features/settings/tabs/PrivacyTab.tsx` — second toggle
- `electron-client/src/layouts/MainLayout.tsx` — mount the disclosure banner
- `electron-client/src/features/servers/useServerEvents.ts` — `Sentry.setTag("connected_servers", ...)`
- `electron-client/vite.config.ts` — `build.sourcemap: true`
- `electron-client/electron-builder.yml` — add `sentry.json` to `extraResources`, exclude `.map` files from package
- `electron-client/.gitignore` — `resources/sentry.json`
- `.github/workflows/electron-release.yml` — DSN bake + source-map upload steps

**LOC budget:** ~350 new, ~50 modified.

## Out of scope (deferred to later versions)

- Usage analytics (PostHog/Plausible) — own brainstorm.
- Performance monitoring (`tracesSampleRate > 0`).
- Session replay (privacy implications need a separate design pass).
- Native Rust error capture via `sentry-rust`.
- In-app "report a bug" feedback widget.
- Self-hosted Sentry instance.
- Live opt-out toggle (no SDK teardown/init mid-session).
- Per-error redaction beyond IP stripping (rely on source-code hygiene
  audit instead).
