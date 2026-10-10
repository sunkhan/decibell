import { create } from "zustand";
import type { HotkeyAction, HotkeyBinding } from "../features/hotkeys/keys";

export type InputMode = "voice_activity" | "push_to_talk";

export const DEFAULT_PTT_RELEASE_DELAY_MS = 100;
export const MAX_PTT_RELEASE_DELAY_MS = 1000;

/// Native listener status (`hotkeys_status` event / `hotkeys_configure`).
export interface HotkeysStatus {
  backend: "portal" | "x11" | "xwayland" | "windows" | "electron" | "none";
  state: "idle" | "starting" | "active" | "error";
  detail: string | null;
  /// The listener sees mouse buttons.
  mouse: boolean;
  /// The listener misses keys while Decibell is focused; the renderer
  /// matches window key events itself.
  focusedFallback: boolean;
  /// The desktop's own shortcut editor can be opened (portal v2).
  canConfigure: boolean;
  /// Binding id → the trigger the desktop actually assigned (portal).
  triggers: Record<string, string>;
  /// Binding ids the listener couldn't register (X11: another app
  /// already grabs that combo).
  failed: string[];
}

interface HotkeysState {
  bindings: HotkeyBinding[];
  inputMode: InputMode;
  pttReleaseDelayMs: number;
  status: HotkeysStatus | null;
  /// macOS: binding ids `globalShortcut` refused (taken / unrepresentable).
  acceleratorFailures: string[];
  /// The Keybinds tab is capturing a combo; hotkeys are paused meanwhile.
  recording: boolean;
  setBindings: (bindings: HotkeyBinding[]) => void;
  addBinding: (binding: HotkeyBinding) => void;
  updateBinding: (id: string, patch: { action?: HotkeyAction; keys?: string[]; id?: string }) => void;
  removeBinding: (id: string) => void;
  setInputMode: (mode: InputMode) => void;
  setPttReleaseDelayMs: (ms: number) => void;
  setStatus: (status: HotkeysStatus) => void;
  setAcceleratorFailures: (ids: string[]) => void;
  setRecording: (recording: boolean) => void;
}

export const useHotkeysStore = create<HotkeysState>((set) => ({
  bindings: [],
  inputMode: "voice_activity",
  pttReleaseDelayMs: DEFAULT_PTT_RELEASE_DELAY_MS,
  status: null,
  acceleratorFailures: [],
  recording: false,
  setBindings: (bindings) => set({ bindings }),
  addBinding: (binding) => set((s) => ({ bindings: [...s.bindings, binding] })),
  updateBinding: (id, patch) =>
    set((s) => ({ bindings: s.bindings.map((b) => (b.id === id ? { ...b, ...patch } : b)) })),
  removeBinding: (id) => set((s) => ({ bindings: s.bindings.filter((b) => b.id !== id) })),
  setInputMode: (inputMode) => set({ inputMode }),
  setPttReleaseDelayMs: (ms) =>
    set({ pttReleaseDelayMs: Math.max(0, Math.min(MAX_PTT_RELEASE_DELAY_MS, Math.round(ms))) }),
  setStatus: (status) => set({ status }),
  setAcceleratorFailures: (acceleratorFailures) => set({ acceleratorFailures }),
  setRecording: (recording) => set({ recording }),
}));
