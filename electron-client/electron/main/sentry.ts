import * as Sentry from "@sentry/electron/main";
import { app } from "electron";
import * as fs from "node:fs";
import * as path from "node:path";

// Owns all main-process Sentry interaction. Three gates protect against
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
    let raw = fs.readFileSync(p, "utf8");
    // Strip UTF-8 BOM if present. PowerShell 5.x's `Out-File -Encoding
    // utf8` writes UTF-8 *with* BOM by default, and JSON.parse rejects
    // the leading ﻿. Get-Content silently strips it on read which
    // makes the file look fine — making this a confusing failure mode
    // for anyone who hand-writes the file on Windows. CI's bash echo
    // never adds a BOM so this is purely defensive for local builds.
    if (raw.charCodeAt(0) === 0xFEFF) raw = raw.slice(1);
    const j = JSON.parse(raw) as DsnJson;
    return j.dsn && j.dsn.length > 0 ? j.dsn : null;
  } catch (e) {
    console.warn("[sentry] loadDsn failed:", e);
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
