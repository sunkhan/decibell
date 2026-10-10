// Global hotkeys, renderer half. Started once at boot (main.tsx):
//  - keeps native in step with the store (bindings → `hotkeys_configure`,
//    input mode → `set_input_mode`, recording → `hotkeys_set_paused`) and,
//    on macOS, main's accelerators;
//  - runs press actions when native reports them (`hotkey_action`) —
//    push-to-talk / push-to-mute never come through here, native gates
//    the mic itself;
//  - the focused fallback: where the listener can't see keys typed into
//    Decibell (XWayland, macOS hold actions), match window key events
//    here and inject them.
// Design: docs/superpowers/specs/2026-10-10-global-hotkeys-design.md.

import { invoke, listen } from "../../lib/ipc";
import { useHotkeysStore, type HotkeysStatus } from "../../stores/hotkeysStore";
import { useCallStore } from "../../stores/callStore";
import { acceptCall, declineCall } from "../call/callActions";
import { leaveVoiceOrCall, toggleDeafen, toggleMute } from "../voice/voiceActions";
import {
  ComboMatcher,
  isHoldAction,
  isHotkeyAction,
  loadKeyboardLayout,
  mouseKey,
  type Fired,
  type HotkeyBinding,
} from "./keys";

let started = false;

export function startHotkeyRuntime(): void {
  if (started) return;
  started = true;
  loadKeyboardLayout();

  void listen<{ action: string; pressed: boolean }>("hotkey_action", (e) =>
    runAction(e.payload.action, e.payload.pressed),
  );
  void listen<HotkeysStatus>("hotkeys_status", (e) => {
    useHotkeysStore.getState().setStatus(e.payload);
  });
  invoke<HotkeysStatus>("hotkeys_get_status")
    .then((s) => {
      // An event may already have brought something newer.
      if (!useHotkeysStore.getState().status) useHotkeysStore.getState().setStatus(s);
    })
    .catch((e) => console.warn("[hotkeys] status unavailable:", e));
  pushInputMode();

  useHotkeysStore.subscribe((s, prev) => {
    if (s.bindings !== prev.bindings) void pushBindings();
    if (s.inputMode !== prev.inputMode || s.pttReleaseDelayMs !== prev.pttReleaseDelayMs) {
      pushInputMode();
    }
    if (s.recording !== prev.recording) {
      invoke("hotkeys_set_paused", { paused: s.recording }).catch(console.error);
    }
    if (s.bindings !== prev.bindings || s.status !== prev.status || s.recording !== prev.recording) {
      syncFallback();
    }
  });
}

function runAction(action: string, pressed: boolean): void {
  if (!pressed || !isHotkeyAction(action) || isHoldAction(action)) return;
  switch (action) {
    case "toggle_mute":
      toggleMute();
      break;
    case "toggle_deafen":
      toggleDeafen();
      break;
    case "leave_voice":
      void leaveVoiceOrCall();
      break;
    case "answer_call":
      if (useCallStore.getState().status === "incoming") void acceptCall();
      break;
    case "decline_call":
      if (useCallStore.getState().status === "incoming") void declineCall();
      break;
  }
}

function pushInputMode(): void {
  const { inputMode, pttReleaseDelayMs } = useHotkeysStore.getState();
  invoke("set_input_mode", {
    pushToTalk: inputMode === "push_to_talk",
    releaseDelayMs: pttReleaseDelayMs,
  }).catch(console.error);
}

// One configure in flight at a time; edits made meanwhile collapse into
// a single follow-up with the newest set.
let pushing = false;
let dirty = false;

async function pushBindings(): Promise<void> {
  dirty = true;
  if (pushing) return;
  pushing = true;
  while (dirty) {
    dirty = false;
    const bindings = useHotkeysStore
      .getState()
      .bindings.filter((b) => b.keys.length > 0)
      .map(({ id, action, keys }) => ({ id, action, keys }));
    await invoke("hotkeys_configure", { bindings }).catch((e) =>
      console.error("[hotkeys] configure failed:", e),
    );
    if (window.decibell.platform === "darwin") {
      const failed = await window.decibell.hotkeys
        .setAccelerators(bindings)
        .catch(() => bindings.map((b) => b.id));
      useHotkeysStore.getState().setAcceleratorFailures(failed);
    }
  }
  pushing = false;
}

// ── Focused fallback ─────────────────────────────────────────────────

const matcher = new ComboMatcher();
let fallbackBindings: HotkeyBinding[] | null = null;
let fallbackHoldOnly = false;

function inject(fired: Fired[]): void {
  for (const f of fired) {
    invoke("hotkeys_inject", { action: f.action, pressed: f.pressed, source: f.id }).catch(
      console.error,
    );
  }
}

const onKeyDown = (e: KeyboardEvent) => {
  if (e.code) inject(matcher.key(e.code, true));
};
const onKeyUp = (e: KeyboardEvent) => {
  if (e.code) inject(matcher.key(e.code, false));
};
const onMouseDown = (e: MouseEvent) => {
  const k = mouseKey(e.button);
  if (k) inject(matcher.key(k, true));
};
const onMouseUp = (e: MouseEvent) => {
  const k = mouseKey(e.button);
  if (k) inject(matcher.key(k, false));
};
// Keys released while the window is unfocused never reach us.
const onBlur = () => inject(matcher.releaseAll());

function syncFallback(): void {
  const { status, recording, bindings } = useHotkeysStore.getState();
  const on = !!status?.focusedFallback && !recording;
  const holdOnly = status?.backend === "electron";
  const next = on ? bindings : null;
  // Re-arming releases whatever is held, so only on a real change.
  if (next === fallbackBindings && holdOnly === fallbackHoldOnly) return;
  const wasOn = fallbackBindings !== null;
  fallbackBindings = next;
  fallbackHoldOnly = holdOnly;
  // macOS: accelerators already deliver press actions everywhere.
  const armed = (next ?? []).filter((b) => !holdOnly || isHoldAction(b.action));
  inject(matcher.setBindings(armed));
  if (on && !wasOn) {
    window.addEventListener("keydown", onKeyDown, true);
    window.addEventListener("keyup", onKeyUp, true);
    window.addEventListener("mousedown", onMouseDown, true);
    window.addEventListener("mouseup", onMouseUp, true);
    window.addEventListener("blur", onBlur);
  } else if (!on && wasOn) {
    window.removeEventListener("keydown", onKeyDown, true);
    window.removeEventListener("keyup", onKeyUp, true);
    window.removeEventListener("mousedown", onMouseDown, true);
    window.removeEventListener("mouseup", onMouseUp, true);
    window.removeEventListener("blur", onBlur);
  }
}
