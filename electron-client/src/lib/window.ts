// Tauri-compatible window-control surface. Mirrors the subset of
// `@tauri-apps/api/window`'s `Window` interface that tauri-client
// uses, so files like Titlebar.tsx, useWindowTitle.ts, ResizeHandles
// port over with a single regex sweep on the import path:
//
//   import { getCurrentWindow } from "@tauri-apps/api/window";
//                                ↓
//   import { getCurrentWindow } from "@/lib/window";
//
// The actual window operations happen in Electron main; preload bridges
// them via contextBridge.

interface DecibellWindow {
  minimize: () => Promise<void>;
  maximize: () => Promise<void>;
  unmaximize: () => Promise<void>;
  toggleMaximize: () => Promise<void>;
  close: () => Promise<void>;
  isMaximized: () => Promise<boolean>;
  setTitle: (title: string) => Promise<void>;
  setFullscreen: (on: boolean) => Promise<void>;
  /// Fires on any resize (including maximize/unmaximize/fullscreen
  /// transitions). Returns an unsubscribe function. Tauri-API parity
  /// returns `Promise<UnlistenFn>`; the shim is sync-resolves to keep
  /// the same await-fn() shape.
  onResized: (cb: () => void) => Promise<() => void>;
}

const w = window.decibell.window;

export function getCurrentWindow(): DecibellWindow {
  return {
    minimize: () => w.minimize(),
    maximize: () => w.maximize(),
    unmaximize: () => w.unmaximize(),
    toggleMaximize: () => w.toggleMaximize(),
    close: () => w.close(),
    isMaximized: () => w.isMaximized(),
    setTitle: (title) => w.setTitle(title),
    setFullscreen: (on) => w.setFullscreen(on),
    onResized: async (cb) => w.onResized(cb),
  };
}
