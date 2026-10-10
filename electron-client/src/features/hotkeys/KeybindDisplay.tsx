// How a keybind reads in Settings (Keybinds tab rows, Audio → Input Mode).

import type { HotkeysStatus } from "../../stores/hotkeysStore";
import { comboLabel, isHoldAction, keyLabel, modifierOf, type HotkeyBinding } from "./keys";

export function KeyChips({ keys }: { keys: string[] }) {
  return (
    <span className="flex flex-wrap items-center gap-1">
      {keys.map((k, i) => (
        <span key={k} className="flex items-center gap-1">
          {i > 0 && <span className="text-[11px] text-text-faint">+</span>}
          <kbd className="rounded-sm border border-border bg-bg-lighter px-1.5 py-0.5 font-meta text-[12px] font-medium text-text-primary">
            {keyLabel(k)}
          </kbd>
        </span>
      ))}
    </span>
  );
}

/// The bound press goes to Decibell only: the desktop's shortcut service
/// (Wayland portal, macOS accelerators) or the X server's grab takes it
/// from the app in front. Windows polling and the focused fallback leave
/// the key alone.
export function isSwallowed(b: HotkeyBinding, status: HotkeysStatus | null): boolean {
  if (b.keys.length === 0) return false;
  switch (status?.backend) {
    case "portal":
    case "x11":
    case "xwayland":
      return true;
    case "electron":
      return !isHoldAction(b.action);
    default:
      return false;
  }
}

/// A combo that types text: no Ctrl / Alt / Super and a character key
/// (Shift alone still types).
export function typesText(keys: string[]): boolean {
  const commandMod = keys.some((k) => {
    const m = ["Control", "Alt", "Meta"].includes(k) ? k : modifierOf(k);
    return m === "Control" || m === "Alt" || m === "Meta";
  });
  return (
    !commandMod &&
    keys.some((k) =>
      /^(Key[A-Z]|Digit[0-9]|Space|Minus|Equal|BracketLeft|BracketRight|Backslash|Semicolon|Quote|Backquote|Comma|Period|Slash|IntlBackslash|Enter|Backspace|Tab)$/.test(k),
    )
  );
}

/// What the desktop actually assigned (portal), when it isn't the combo
/// recorded here — the user can change it in the system settings.
export function assignedTrigger(b: HotkeyBinding, status: HotkeysStatus | null): string | null {
  if (status?.backend !== "portal" || b.keys.length === 0) return null;
  const trigger = status.triggers[b.id];
  if (!trigger) return null;
  const norm = (s: string) => s.replace(/\s/g, "").toLowerCase();
  return norm(trigger) !== norm(comboLabel(b.keys)) ? trigger : null;
}

/// Marks a key the app in front never receives while it's bound.
export function SwallowBadge({ typing }: { typing: boolean }) {
  const why =
    "Your desktop hands this key press to Decibell only, so the game or app you're in won't register it." +
    (typing ? " It won't type in other apps either." : "") +
    " Pick a key your games don't use.";
  return (
    <span
      title={why}
      aria-label={why}
      className="inline-flex shrink-0 items-center gap-1 rounded-sm bg-warning/15 px-1.5 py-0.5 text-[10px] font-semibold uppercase tracking-[0.04em] text-warning"
    >
      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
        <path d="M17.94 17.94A10.07 10.07 0 0 1 12 20c-7 0-11-8-11-8a18.45 18.45 0 0 1 5.06-5.94" />
        <path d="M9.9 4.24A9.12 9.12 0 0 1 12 4c7 0 11 8 11 8a18.5 18.5 0 0 1-2.16 3.19" />
        <path d="M14.12 14.12a3 3 0 1 1-4.24-4.24" />
        <path d="M1 1l22 22" />
      </svg>
      Games won't see it
    </span>
  );
}
