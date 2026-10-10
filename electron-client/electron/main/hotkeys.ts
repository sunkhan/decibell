// Global hotkeys, main-process half. The listeners live in the native
// addon (native/src/hotkeys/); this file covers what only Electron main
// can do:
//
//  - `decibell --hotkey=<action>[:down|:up]` — a second launch forwards
//    the action through the single-instance handshake. The escape hatch
//    for desktops with no global-shortcut support (Sway, GNOME < 48, …):
//    bind the command in the compositor.
//  - macOS: press actions registered as `globalShortcut` accelerators
//    (no Input Monitoring prompt; hold actions work while focused only).
//
// Both end in the addon's `hotkeysInject`, the same dispatcher the native
// listeners use. Design: docs/superpowers/specs/2026-10-10-global-hotkeys-design.md.

import { spawn } from "child_process";
import * as fs from "fs";
import { app, globalShortcut, ipcMain } from "electron";
import { callCommand } from "./addon";

const ACTIONS = new Set([
  "toggle_mute",
  "toggle_deafen",
  "push_to_talk",
  "push_to_mute",
  "leave_voice",
  "answer_call",
  "decline_call",
]);
const HOLD_ACTIONS = new Set(["push_to_talk", "push_to_mute"]);

export type HotkeyArg = { action: string; pressed: boolean };

/// `--hotkey=toggle_mute`, `--hotkey=push_to_talk:down` / `:up`, or the
/// two-token `--hotkey toggle_mute`. A bare hold action means "down".
export function parseHotkeyArg(argv: readonly string[]): HotkeyArg | null {
  let value: string | undefined;
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--hotkey=")) {
      value = a.slice("--hotkey=".length);
      break;
    }
    if (a === "--hotkey") {
      value = argv[i + 1];
      break;
    }
  }
  if (!value) return null;
  const [action, edge] = value.split(":");
  if (!ACTIONS.has(action)) return null;
  if (edge !== undefined && edge !== "down" && edge !== "up") return null;
  if (edge === "up" && !HOLD_ACTIONS.has(action)) return null;
  return { action, pressed: edge !== "up" };
}

export function injectHotkey(arg: HotkeyArg, source: string): void {
  try {
    callCommand("hotkeysInject", { action: arg.action, pressed: arg.pressed, source });
  } catch (e) {
    console.warn("[hotkeys] inject failed:", e);
  }
}

// ── macOS accelerators ─────────────────────────────────────────────────

type Binding = { id: string; action: string; keys: string[] };

const MODIFIER_ACCEL: Record<string, string> = {
  Control: "Control",
  Shift: "Shift",
  Alt: "Alt",
  Meta: "Command",
};

const KEY_ACCEL: Record<string, string> = {
  Space: "Space",
  Enter: "Return",
  Escape: "Escape",
  Backspace: "Backspace",
  Tab: "Tab",
  Delete: "Delete",
  Insert: "Insert",
  Home: "Home",
  End: "End",
  PageUp: "PageUp",
  PageDown: "PageDown",
  ArrowUp: "Up",
  ArrowDown: "Down",
  ArrowLeft: "Left",
  ArrowRight: "Right",
  Minus: "-",
  Equal: "=",
  BracketLeft: "[",
  BracketRight: "]",
  Backslash: "\\",
  Semicolon: ";",
  Quote: "'",
  Backquote: "`",
  Comma: ",",
  Period: ".",
  Slash: "/",
  CapsLock: "Capslock",
  PrintScreen: "PrintScreen",
  NumpadAdd: "numadd",
  NumpadSubtract: "numsub",
  NumpadMultiply: "nummult",
  NumpadDivide: "numdiv",
  NumpadDecimal: "numdec",
  MediaPlayPause: "MediaPlayPause",
  MediaTrackNext: "MediaNextTrack",
  MediaTrackPrevious: "MediaPreviousTrack",
  MediaStop: "MediaStop",
  AudioVolumeUp: "VolumeUp",
  AudioVolumeDown: "VolumeDown",
  AudioVolumeMute: "VolumeMute",
};

/// DOM codes → an Electron accelerator. Null for what accelerators can't
/// express: mouse buttons, modifier-only combos, unmapped keys.
function toAccelerator(keys: string[]): string | null {
  const mods: string[] = [];
  let main: string | null = null;
  for (const k of keys) {
    const mod = MODIFIER_ACCEL[k];
    if (mod) {
      mods.push(mod);
      continue;
    }
    if (main !== null) return null;
    const m =
      /^Key([A-Z])$/.exec(k)?.[1] ??
      /^Digit([0-9])$/.exec(k)?.[1] ??
      (/^F([1-9]|1[0-9]|2[0-4])$/.test(k) ? k : undefined) ??
      (/^Numpad([0-9])$/.test(k) ? `num${k.slice(6)}` : undefined) ??
      KEY_ACCEL[k];
    if (!m) return null;
    main = m;
  }
  return main === null ? null : [...mods, main].join("+");
}

/// Register press-action bindings as global accelerators (macOS only).
/// Returns the ids that couldn't be registered (unrepresentable, or
/// taken by another app).
function setAccelerators(bindings: Binding[]): string[] {
  if (process.platform !== "darwin") return [];
  globalShortcut.unregisterAll();
  const failed: string[] = [];
  for (const b of bindings) {
    if (!ACTIONS.has(b.action) || HOLD_ACTIONS.has(b.action)) continue;
    const accel = toAccelerator(b.keys);
    let ok = false;
    if (accel) {
      try {
        ok = globalShortcut.register(accel, () =>
          injectHotkey({ action: b.action, pressed: true }, b.id),
        );
      } catch (e) {
        console.warn("[hotkeys] accelerator rejected:", accel, e);
      }
    }
    if (!ok) failed.push(b.id);
  }
  return failed;
}

/// What to put in a compositor binding: the packaged binary (AppImage
/// path, or `decibell` when the package put it on PATH). Null in dev.
function launchCommand(): string | null {
  if (!app.isPackaged) return null;
  if (process.env.APPIMAGE) return process.env.APPIMAGE;
  if (process.platform === "linux" && fs.existsSync("/usr/bin/decibell")) return "decibell";
  return process.execPath;
}

// ── Mouse buttons on Wayland ───────────────────────────────────────────
// The desktop's shortcut service only binds keys, so a side button has to
// become a key in the desktop's own settings first (KDE: Mouse → Extra
// Mouse Buttons). The Keybinds tab guides that; these two calls tailor it.

type Desktop = "kde" | "gnome" | "other";

function desktop(): Desktop {
  if (process.platform !== "linux") return "other";
  const d = (process.env.XDG_CURRENT_DESKTOP ?? "").toUpperCase().split(":");
  if (d.includes("KDE")) return "kde";
  if (d.includes("GNOME")) return "gnome";
  return "other";
}

/// KDE only: open System Settings on the Mouse page.
function openMouseSettings(): boolean {
  if (desktop() !== "kde") return false;
  try {
    const child = spawn("systemsettings", ["kcm_mouse"], { detached: true, stdio: "ignore" });
    child.on("error", (e) => console.warn("[hotkeys] systemsettings failed:", e));
    child.unref();
    return true;
  } catch (e) {
    console.warn("[hotkeys] systemsettings failed:", e);
    return false;
  }
}

export function registerHotkeyIpc(): void {
  ipcMain.handle("decibell:hotkeys:desktop", () => desktop());
  ipcMain.handle("decibell:hotkeys:openMouseSettings", () => openMouseSettings());
  ipcMain.handle("decibell:hotkeys:setAccelerators", (_e, bindings: unknown) => {
    if (!Array.isArray(bindings)) return [];
    const valid = bindings.filter(
      (b): b is Binding =>
        !!b &&
        typeof b.id === "string" &&
        typeof b.action === "string" &&
        Array.isArray(b.keys) &&
        b.keys.every((k: unknown) => typeof k === "string"),
    );
    return setAccelerators(valid);
  });
  ipcMain.handle("decibell:hotkeys:launchCommand", () => launchCommand());
  app.on("will-quit", () => globalShortcut.unregisterAll());
}
