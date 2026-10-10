import { useEffect, useState } from "react";
import { invoke } from "../../../lib/ipc";
import { useHotkeysStore, type HotkeysStatus } from "../../../stores/hotkeysStore";
import { saveSettings } from "../saveSettings";
import {
  HOTKEY_ACTIONS,
  actionLabel,
  isHoldAction,
  isMouseKey,
  keyLabel,
  modifierOf,
  mouseKey,
  newBindingId,
  normalizeCombo,
  onKeyboardLayout,
  portalCanBind,
  sameCombo,
  type HotkeyAction,
  type HotkeyBinding,
} from "../../hotkeys/keys";
import {
  KeyChips,
  SwallowBadge,
  assignedTrigger,
  isSwallowed,
  typesText,
} from "../../hotkeys/KeybindDisplay";

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <div className="mb-2.5 pl-0.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
      {children}
    </div>
  );
}

/// Capture one combo from the window. Keys finish on the first
/// non-modifier key; a modifier-only combo finishes when everything is
/// released; a middle / back / forward click finishes with the held
/// modifiers. Esc (alone) cancels, as does losing focus. Capture phase +
/// stopPropagation keeps the keys from the rest of the app (Esc would
/// otherwise close the modal, Back would navigate).
function recordCombo(onDone: (keys: string[] | null) => void): () => void {
  const held = new Set<string>();
  const peak = new Set<string>();
  let done = false;
  const swallow = (e: Event) => {
    e.preventDefault();
    e.stopPropagation();
  };
  const finish = (keys: string[] | null) => {
    if (done) return;
    done = true;
    cleanup();
    onDone(keys);
  };
  const onKeyDown = (e: KeyboardEvent) => {
    swallow(e);
    if (!e.code || e.repeat) return;
    if (e.code === "Escape" && held.size === 0) return finish(null);
    held.add(e.code);
    peak.add(e.code);
    if (!modifierOf(e.code)) finish(normalizeCombo([...held]));
  };
  const onKeyUp = (e: KeyboardEvent) => {
    swallow(e);
    held.delete(e.code);
    if (held.size === 0 && peak.size > 0) finish(normalizeCombo([...peak]));
  };
  const onMouseDown = (e: MouseEvent) => {
    const k = mouseKey(e.button);
    if (!k) return;
    swallow(e);
    finish(normalizeCombo([...[...held].filter((h) => modifierOf(h)), k]));
  };
  // Back / forward buttons navigate on release unless stopped.
  const onMouseUp = (e: MouseEvent) => {
    if (mouseKey(e.button)) swallow(e);
  };
  const onBlur = () => finish(null);
  const cleanup = () => {
    window.removeEventListener("keydown", onKeyDown, true);
    window.removeEventListener("keyup", onKeyUp, true);
    window.removeEventListener("mousedown", onMouseDown, true);
    window.removeEventListener("mouseup", onMouseUp, true);
    window.removeEventListener("blur", onBlur);
  };
  window.addEventListener("keydown", onKeyDown, true);
  window.addEventListener("keyup", onKeyUp, true);
  window.addEventListener("mousedown", onMouseDown, true);
  window.addEventListener("mouseup", onMouseUp, true);
  window.addEventListener("blur", onBlur);
  return () => {
    done = true;
    cleanup();
  };
}

/// Why a binding won't work as recorded on this machine, if it won't.
function bindingProblem(
  b: HotkeyBinding,
  bindings: HotkeyBinding[],
  status: HotkeysStatus | null,
  acceleratorFailures: string[],
): string | null {
  if (b.keys.length === 0) return null;
  const twin = bindings.find((o) => o.id !== b.id && sameCombo(o.keys, b.keys));
  if (twin) return `Same keys as “${actionLabel(twin.action)}”.`;
  if (status?.backend === "portal" && b.keys.some(isMouseKey)) {
    return "Your desktop's shortcut system only takes keys. Click the keys and press the button to see how to remap it.";
  }
  if (status?.backend === "portal" && !portalCanBind(b.keys)) {
    return "Your desktop's shortcut system needs a key, optionally with modifiers — not a modifier on its own.";
  }
  if (status?.backend === "electron" && !isHoldAction(b.action)) {
    if (b.keys.some(isMouseKey) || b.keys.every((k) => modifierOf(k) || ["Control", "Shift", "Alt", "Meta"].includes(k))) {
      return "macOS global shortcuts need a key, optionally with modifiers.";
    }
    if (acceleratorFailures.includes(b.id)) {
      return "macOS refused this shortcut — another app may already use it.";
    }
  }
  if (status?.failed?.includes(b.id)) {
    return "Another app already uses this shortcut.";
  }
  if (b.keys.some(isMouseKey) && status && !status.mouse && !status.focusedFallback) {
    return "Mouse buttons aren't available here.";
  }
  return null;
}

type Desktop = "kde" | "gnome" | "other";

/// The desktop's shortcut service (Wayland portal) binds keys only, and
/// nothing outside the compositor may watch a mouse — so a side button
/// becomes a key combination in the desktop's own settings first, and
/// Decibell binds that.
function MouseButtonGuide({
  button,
  desktop,
  onRecordAgain,
  onDismiss,
}: {
  button: string;
  desktop: Desktop;
  onRecordAgain: () => void;
  onDismiss: () => void;
}) {
  const name = keyLabel(button);
  const [openFailed, setOpenFailed] = useState(false);
  const openSettings = () => {
    window.decibell.hotkeys
      .openMouseSettings()
      .then((ok) => setOpenFailed(!ok))
      .catch(() => setOpenFailed(true));
  };
  return (
    <div className="mt-3 rounded-md border border-border-divider bg-bg-lighter px-3.5 py-3 text-[12px] leading-[1.6] text-text-secondary">
      <div className="text-[13px] font-medium text-text-primary">Turn {name} into a key first</div>
      <div className="mt-0.5 text-text-muted">
        Your desktop's shortcut system only takes keys, and only the desktop can see your mouse.
      </div>
      {desktop === "kde" ? (
        <ol className="mt-2 list-decimal space-y-1 pl-4">
          <li>
            Open your mouse settings and go to <span className="font-medium text-text-primary">Extra Mouse Buttons</span>.
          </li>
          <li>
            Click <span className="font-medium text-text-primary">Add Binding</span>, press {name}, then type a
            combination you don't use anywhere else. One with the Meta key, like Meta + F9, stays out of your
            games' way.
          </li>
          <li>Come back and press {name} here — Decibell records that combination.</li>
        </ol>
      ) : desktop === "gnome" ? (
        <div className="mt-2">
          GNOME can't remap mouse buttons on its own. A tool like Piper (for gaming mice) or input-remapper can turn{" "}
          {name} into a key combination — then press it here again.
        </div>
      ) : (
        <div className="mt-2">
          If your desktop's input settings can remap {name} to a key combination, do that and press it here again. Sway
          and Hyprland can also bind the button to the command line below.
        </div>
      )}
      {openFailed && <div className="mt-2 text-danger">Couldn't open System Settings — open it from your app menu.</div>}
      <div className="mt-3 flex items-center gap-2">
        {desktop === "kde" && (
          <button
            type="button"
            onClick={openSettings}
            className="rounded-sm bg-accent px-3 py-1.5 text-[12px] font-semibold text-on-accent hover:bg-accent-hover"
          >
            Open mouse settings
          </button>
        )}
        <button
          type="button"
          onClick={onRecordAgain}
          className="rounded-sm border border-border px-3 py-1.5 text-[12px] font-medium text-text-secondary transition-colors hover:border-accent/40 hover:text-text-primary"
        >
          Record again
        </button>
        <button
          type="button"
          onClick={onDismiss}
          className="rounded-sm px-2 py-1.5 text-[12px] font-medium text-text-muted transition-colors hover:text-text-primary"
        >
          Dismiss
        </button>
      </div>
    </div>
  );
}

function BindingRow({
  binding,
  recording,
  onRecord,
  onAction,
  onRemove,
  status,
  problem,
  guide,
}: {
  binding: HotkeyBinding;
  recording: boolean;
  onRecord: () => void;
  onAction: (action: HotkeyAction) => void;
  onRemove: () => void;
  status: HotkeysStatus | null;
  problem: string | null;
  /// Shown under the row instead of the usual notes.
  guide?: React.ReactNode;
}) {
  const assigned = assignedTrigger(binding, status);
  const portalNote =
    status?.backend !== "portal" || binding.keys.length === 0 || problem
      ? null
      : assigned
        ? `Your desktop assigned ${assigned}.`
        : !status.triggers[binding.id] && status.state === "active"
          ? "No key assigned in your desktop's shortcut settings."
          : null;
  const swallowed = !problem && isSwallowed(binding, status);

  return (
    <div className="rounded-md border border-border-divider bg-bg-light px-4 py-3">
      <div className="flex items-center gap-3">
        <div className="relative w-[210px] shrink-0">
          <select
            value={binding.action}
            onChange={(e) => onAction(e.target.value as HotkeyAction)}
            aria-label="Action"
            className="w-full appearance-none rounded-md border border-border bg-bg-lighter px-3 py-2 pr-9 text-[13px] text-text-primary outline-none transition-all hover:border-text-faint focus:border-accent focus:shadow-ring"
          >
            {HOTKEY_ACTIONS.map((a) => (
              <option key={a.id} value={a.id} className="bg-bg-lighter">
                {a.label}
              </option>
            ))}
          </select>
          <div className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 text-text-muted">
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round"><path d="M6 9l6 6 6-6" /></svg>
          </div>
        </div>
        <button
          type="button"
          onClick={onRecord}
          title={recording ? undefined : "Click to change"}
          className={`flex min-h-[38px] flex-1 items-center rounded-md border px-3 py-1.5 text-left text-[13px] transition-all ${
            recording
              ? "border-accent bg-accent-soft text-accent-bright shadow-ring"
              : "border-border bg-bg-lighter text-text-primary hover:border-accent/40"
          }`}
        >
          {recording ? (
            <span>Press keys…</span>
          ) : binding.keys.length > 0 ? (
            <span className="flex w-full items-center justify-between gap-3">
              <KeyChips keys={binding.keys} />
              {swallowed && <SwallowBadge typing={typesText(binding.keys)} />}
            </span>
          ) : (
            <span className="text-text-muted">Click to record</span>
          )}
        </button>
        <button
          type="button"
          onClick={onRemove}
          title="Remove keybind"
          aria-label="Remove keybind"
          className="flex h-[38px] w-[38px] shrink-0 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-danger"
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
            <path d="M18 6L6 18M6 6l12 12" />
          </svg>
        </button>
      </div>
      {recording && (
        <div className="mt-2 text-[12px] leading-[1.55] text-text-muted">
          {status?.backend === "portal"
            ? "Press a key or a combo. Mouse side buttons need a quick remap in your desktop first — press one to see how. Esc cancels."
            : status?.backend === "electron"
              ? "Press a key or a combo. Esc cancels."
              : "Press a key or a combo, or a middle / back / forward mouse button. Esc cancels."}
        </div>
      )}
      {!recording && guide}
      {!recording && !guide && problem && (
        <div className="mt-2 text-[12px] leading-[1.55] text-danger">{problem}</div>
      )}
      {!recording && !guide && portalNote && (
        <div className="mt-2 text-[12px] leading-[1.55] text-text-muted">{portalNote}</div>
      )}
      {!recording && !guide && swallowed && typesText(binding.keys) && (
        <div className="mt-2 text-[12px] leading-[1.55] text-text-muted">
          While bound, this key won't type in other apps.
        </div>
      )}
    </div>
  );
}

function backendSummary(status: HotkeysStatus | null): string {
  switch (status?.backend) {
    case "portal":
      return "Your desktop runs these shortcuts and tells Decibell only when one of them fires. New keybinds are confirmed in a desktop dialog, and you can change their keys in your system settings too.";
    case "x11":
      return "Keybinds work in every app, including games. Decibell is only told about your bound keys — nothing else you type.";
    case "xwayland":
      return "Your desktop has no global shortcut service, so keybinds work while Decibell or an X11 app (most games) is focused, and Decibell is only told about your bound keys. For everything else, bind the command line below in your window manager.";
    case "windows":
      return "Keybinds work in every app, including games. Decibell checks only your bound keys (and Ctrl, Shift, Alt and Win when a combo needs them), never anything else you type, and the keys still reach the app you're in. A game running as administrator may hide its keys unless Decibell runs as administrator too.";
    case "electron":
      return "Mute, deafen, leave and call keybinds work in every app; macOS tells Decibell only when one of them fires. Push to talk and push to mute work while Decibell is focused.";
    case "none":
      return "Global keys aren't available here, so keybinds work while Decibell is focused.";
    default:
      return "";
  }
}

export default function KeybindsTab() {
  const bindings = useHotkeysStore((s) => s.bindings);
  const status = useHotkeysStore((s) => s.status);
  const inputMode = useHotkeysStore((s) => s.inputMode);
  const acceleratorFailures = useHotkeysStore((s) => s.acceleratorFailures);
  const [recordingId, setRecordingId] = useState<string | null>(null);
  const [command, setCommand] = useState("decibell");
  const [, relabel] = useState(0);
  const [configureError, setConfigureError] = useState<string | null>(null);
  const [desktop, setDesktop] = useState<Desktop>("other");
  /// A side button recorded where the desktop can't bind it: guide the remap.
  const [mouseGuide, setMouseGuide] = useState<{ id: string; button: string } | null>(null);

  useEffect(() => onKeyboardLayout(() => relabel((n) => n + 1)), []);
  useEffect(() => {
    window.decibell.hotkeys.desktop().then(setDesktop).catch(() => {});
  }, []);
  useEffect(() => {
    window.decibell.hotkeys
      .launchCommand()
      .then((c) => c && setCommand(c.includes(" ") ? `"${c}"` : c))
      .catch(() => {});
  }, []);

  // One recorder at a time; native swallows hotkeys while it runs.
  useEffect(() => {
    if (!recordingId) return;
    const store = useHotkeysStore.getState();
    store.setRecording(true);
    const stop = recordCombo((keys) => {
      const s = useHotkeysStore.getState();
      const b = s.bindings.find((x) => x.id === recordingId);
      const button = keys?.find(isMouseKey);
      if (b && button && s.status?.backend === "portal") {
        // Keep whatever the row had; the guide explains the remap.
        setMouseGuide({ id: recordingId, button });
      } else if (keys && b) {
        // A fresh id: the desktop keeps its own assignment per id, so a
        // new combo only takes effect under a new one.
        s.updateBinding(recordingId, { keys, id: newBindingId() });
      } else if (b && b.keys.length === 0) {
        s.removeBinding(recordingId);
      }
      setRecordingId(null);
      saveSettings();
    });
    return () => {
      stop();
      useHotkeysStore.getState().setRecording(false);
    };
  }, [recordingId]);

  const addBinding = () => {
    const bound = new Set(bindings.map((b) => b.action));
    const action: HotkeyAction =
      inputMode === "push_to_talk" && !bound.has("push_to_talk")
        ? "push_to_talk"
        : HOTKEY_ACTIONS.find((a) => !bound.has(a.id))?.id ?? "toggle_mute";
    const id = newBindingId();
    useHotkeysStore.getState().addBinding({ id, action, keys: [] });
    startRecording(id);
  };

  const setAction = (id: string, action: HotkeyAction) => {
    useHotkeysStore.getState().updateBinding(id, { action });
    saveSettings();
  };

  const startRecording = (id: string | null) => {
    setMouseGuide(null);
    setRecordingId(id);
  };

  const dismissGuide = () => {
    const id = mouseGuide?.id;
    setMouseGuide(null);
    // A new row that never got a key isn't worth keeping.
    if (id && useHotkeysStore.getState().bindings.find((b) => b.id === id)?.keys.length === 0) {
      useHotkeysStore.getState().removeBinding(id);
    }
  };

  const remove = (id: string) => {
    if (mouseGuide?.id === id) setMouseGuide(null);
    if (recordingId === id) setRecordingId(null);
    useHotkeysStore.getState().removeBinding(id);
    saveSettings();
  };

  const openSystemSettings = () => {
    setConfigureError(null);
    invoke("hotkeys_open_system_settings").catch((e) => setConfigureError(String(e)));
  };

  const hasPttKey = bindings.some((b) => b.action === "push_to_talk" && b.keys.length > 0);
  const isLinux = window.decibell.platform === "linux";

  return (
    <div className="flex flex-col gap-6">
      <div>
        <SectionLabel>On this computer</SectionLabel>
        <div className="rounded-md border border-border-divider bg-bg-light px-4 py-3.5">
          <div className="text-[13px] leading-[1.6] text-text-secondary">{backendSummary(status)}</div>
          {bindings.some((b) => isSwallowed(b, status)) && (
            <div className="mt-2.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[12px] leading-[1.55] text-text-muted">
              <SwallowBadge typing={false} />
              <span>
                Here, a bound key press goes to Decibell only — the game or app you're in won't register it. Pick keys
                your games don't use.
              </span>
            </div>
          )}
          {status?.state === "starting" && status.backend === "portal" && (
            <div className="mt-2 text-[12px] text-text-muted">Waiting for your desktop to confirm…</div>
          )}
          {status?.state === "error" && status.detail && (
            <div className="mt-2 text-[12px] leading-[1.55] text-danger">{status.detail}</div>
          )}
          {status?.backend === "portal" && status.canConfigure && status.state === "active" && (
            <button
              type="button"
              onClick={openSystemSettings}
              className="mt-3 rounded-sm border border-border px-3 py-1.5 text-[12px] font-medium text-text-secondary transition-colors hover:border-accent/40 hover:text-text-primary"
            >
              Open system shortcut settings
            </button>
          )}
          {configureError && <div className="mt-2 text-[12px] text-danger">{configureError}</div>}
        </div>
      </div>

      <div className="flex flex-col gap-2.5">
        <SectionLabel>Your keybinds</SectionLabel>
        {bindings.length === 0 && (
          <div className="rounded-md border border-dashed border-border-divider px-4 py-6 text-center text-[13px] text-text-muted">
            No keybinds yet.
          </div>
        )}
        {bindings.map((b) => (
          <BindingRow
            key={b.id}
            binding={b}
            recording={recordingId === b.id}
            onRecord={() => startRecording(recordingId === b.id ? null : b.id)}
            onAction={(a) => setAction(b.id, a)}
            onRemove={() => remove(b.id)}
            status={status}
            problem={bindingProblem(b, bindings, status, acceleratorFailures)}
            guide={
              mouseGuide?.id === b.id ? (
                <MouseButtonGuide
                  button={mouseGuide.button}
                  desktop={desktop}
                  onRecordAgain={() => startRecording(b.id)}
                  onDismiss={dismissGuide}
                />
              ) : undefined
            }
          />
        ))}
        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={addBinding}
            className="rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover"
          >
            Add keybind
          </button>
          {hasPttKey && inputMode !== "push_to_talk" && (
            <span className="text-[12px] text-text-muted">
              Push to talk keys work once Input Mode is Push to talk.{" "}
              <button
                type="button"
                onClick={() => {
                  useHotkeysStore.getState().setInputMode("push_to_talk");
                  saveSettings();
                }}
                className="font-medium text-accent-bright hover:underline"
              >
                Switch now
              </button>
            </span>
          )}
          {!hasPttKey && inputMode === "push_to_talk" && (
            <span className="text-[12px] text-danger">
              Input Mode is Push to talk, but no push to talk key is bound.
            </span>
          )}
        </div>
      </div>

      {isLinux && (
        <details className="group rounded-md border border-border-divider bg-bg-light px-4 py-3">
          <summary className="cursor-pointer select-none text-[13px] font-medium text-text-secondary group-open:mb-2.5">
            Command line
          </summary>
          <div className="text-[12px] leading-[1.6] text-text-muted">
            Runs an action in the open Decibell, for window managers that bind commands
            (Sway, Hyprland, i3…). Hold actions take <code>:down</code> on press and{" "}
            <code>:up</code> on release.
          </div>
          <div className="mt-2.5 flex flex-col gap-1">
            {HOTKEY_ACTIONS.map((a) => (
              <code
                key={a.id}
                className="select-text rounded-sm bg-bg-lighter px-2 py-1 font-meta text-[12px] text-text-primary"
              >
                {command} --hotkey={a.id}
                {a.hold ? ":down" : ""}
              </code>
            ))}
          </div>
        </details>
      )}
    </div>
  );
}
