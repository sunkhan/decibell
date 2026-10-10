// Keybind model shared by the Keybinds tab, the recorder and the focused
// fallback. Keys are DOM `KeyboardEvent.code`s (physical, layout-free),
// generic `Control` / `Shift` / `Alt` / `Meta` in a combo with a real key,
// sided codes (`ControlRight`) only in modifier-only combos, and
// `Mouse3`..`Mouse5`. native/src/hotkeys/ speaks the same spelling.

export type HotkeyAction =
  | "toggle_mute"
  | "toggle_deafen"
  | "push_to_talk"
  | "push_to_mute"
  | "leave_voice"
  | "answer_call"
  | "decline_call";

export interface HotkeyBinding {
  id: string;
  action: HotkeyAction;
  keys: string[];
}

export const HOTKEY_ACTIONS: { id: HotkeyAction; label: string; hold: boolean }[] = [
  { id: "push_to_talk", label: "Push to talk", hold: true },
  { id: "toggle_mute", label: "Toggle mute", hold: false },
  { id: "toggle_deafen", label: "Toggle deafen", hold: false },
  { id: "push_to_mute", label: "Push to mute", hold: true },
  { id: "leave_voice", label: "Leave voice / hang up", hold: false },
  { id: "answer_call", label: "Answer call", hold: false },
  { id: "decline_call", label: "Decline call", hold: false },
];

export function isHotkeyAction(v: unknown): v is HotkeyAction {
  return HOTKEY_ACTIONS.some((a) => a.id === v);
}

export function isHoldAction(action: HotkeyAction): boolean {
  return action === "push_to_talk" || action === "push_to_mute";
}

export function actionLabel(action: HotkeyAction): string {
  return HOTKEY_ACTIONS.find((a) => a.id === action)?.label ?? action;
}

export function newBindingId(): string {
  return crypto.randomUUID();
}

type Modifier = "Control" | "Shift" | "Alt" | "Meta";
const MODIFIER_ORDER: Modifier[] = ["Control", "Alt", "Shift", "Meta"];

/// The modifier a physical key belongs to (sided codes only).
export function modifierOf(code: string): Modifier | null {
  switch (code) {
    case "ControlLeft":
    case "ControlRight":
      return "Control";
    case "ShiftLeft":
    case "ShiftRight":
      return "Shift";
    case "AltLeft":
    case "AltRight":
      return "Alt";
    case "MetaLeft":
    case "MetaRight":
      return "Meta";
    default:
      return null;
  }
}

function isGenericModifier(key: string): key is Modifier {
  return key === "Control" || key === "Shift" || key === "Alt" || key === "Meta";
}

export function isMouseKey(key: string): boolean {
  return key === "Mouse3" || key === "Mouse4" || key === "Mouse5";
}

/// `MouseEvent.button` → key name. Left / right clicks can't be bound.
export function mouseKey(button: number): string | null {
  return button === 1 ? "Mouse3" : button === 3 ? "Mouse4" : button === 4 ? "Mouse5" : null;
}

/// What the recorder saw → a binding's keys. With a real key, modifiers
/// go generic (either side); a modifier-only combo keeps its sides.
export function normalizeCombo(physical: string[]): string[] {
  const mains = physical.filter((k) => !modifierOf(k));
  if (mains.length === 0) {
    const order = (k: string) => MODIFIER_ORDER.indexOf(modifierOf(k)!);
    return [...new Set(physical)].sort((a, b) => order(a) - order(b) || a.localeCompare(b));
  }
  const mods = MODIFIER_ORDER.filter((m) => physical.some((k) => modifierOf(k) === m));
  return [...mods, mains[mains.length - 1]];
}

export function sameCombo(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((k) => b.includes(k));
}

/// The desktop's key grab can only express modifiers + one keyboard key.
export function portalCanBind(keys: string[]): boolean {
  return !keys.some(isMouseKey) && keys.some((k) => !isGenericModifier(k) && !modifierOf(k));
}

// ── Labels ─────────────────────────────────────────────────────────────

const platform = typeof window !== "undefined" ? window.decibell?.platform : undefined;

const MODIFIER_LABEL: Record<Modifier, string> = {
  Control: "Ctrl",
  Shift: "Shift",
  Alt: platform === "darwin" ? "Option" : "Alt",
  Meta: platform === "darwin" ? "Cmd" : platform === "win32" ? "Win" : "Super",
};

const NAMED: Record<string, string> = {
  Space: "Space",
  Enter: "Enter",
  Escape: "Esc",
  Backspace: "Backspace",
  Tab: "Tab",
  CapsLock: "Caps Lock",
  Insert: "Insert",
  Delete: "Delete",
  Home: "Home",
  End: "End",
  PageUp: "Page Up",
  PageDown: "Page Down",
  ArrowUp: "↑",
  ArrowDown: "↓",
  ArrowLeft: "←",
  ArrowRight: "→",
  PrintScreen: "Print Screen",
  ScrollLock: "Scroll Lock",
  Pause: "Pause",
  NumLock: "Num Lock",
  ContextMenu: "Menu",
  NumpadAdd: "Num +",
  NumpadSubtract: "Num −",
  NumpadMultiply: "Num ×",
  NumpadDivide: "Num ÷",
  NumpadDecimal: "Num .",
  NumpadEnter: "Num Enter",
  MediaPlayPause: "Play/Pause",
  MediaStop: "Stop",
  MediaTrackNext: "Next Track",
  MediaTrackPrevious: "Previous Track",
  AudioVolumeMute: "Volume Mute",
  AudioVolumeUp: "Volume Up",
  AudioVolumeDown: "Volume Down",
  Mouse3: "Middle Mouse",
  Mouse4: "Mouse 4",
  Mouse5: "Mouse 5",
};

/// US-layout fallbacks for when the layout map isn't available.
const US: Record<string, string> = {
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
  IntlBackslash: "<",
};

let layoutMap: Map<string, string> | null = null;
const layoutListeners = new Set<() => void>();

/// Load the keyboard layout once so labels show what the key types
/// (`Z` on a German keyboard's Y position). Chromium-only API.
export function loadKeyboardLayout(): void {
  const kb = (navigator as Navigator & {
    keyboard?: { getLayoutMap?: () => Promise<Map<string, string>> };
  }).keyboard;
  kb?.getLayoutMap?.()
    .then((map) => {
      layoutMap = new Map(map);
      for (const l of layoutListeners) l();
    })
    .catch(() => {});
}

export function onKeyboardLayout(listener: () => void): () => void {
  layoutListeners.add(listener);
  return () => layoutListeners.delete(listener);
}

export function keyLabel(key: string): string {
  if (isGenericModifier(key)) return MODIFIER_LABEL[key];
  const mod = modifierOf(key);
  if (mod) return `${key.endsWith("Right") ? "Right" : "Left"} ${MODIFIER_LABEL[mod]}`;
  if (NAMED[key]) return NAMED[key];
  const typed = layoutMap?.get(key);
  if (typed && typed.trim()) return typed.length === 1 ? typed.toUpperCase() : typed;
  const letter = /^Key([A-Z])$/.exec(key)?.[1];
  if (letter) return letter;
  const digit = /^Digit([0-9])$/.exec(key)?.[1];
  if (digit) return digit;
  const numpad = /^Numpad([0-9])$/.exec(key)?.[1];
  if (numpad) return `Num ${numpad}`;
  return US[key] ?? key;
}

export function comboLabel(keys: string[]): string {
  return keys.map(keyLabel).join(" + ");
}

// ── Focused-fallback matcher (mirror of native/src/hotkeys/matcher.rs) ──

export interface Fired {
  id: string;
  action: HotkeyAction;
  pressed: boolean;
}

export class ComboMatcher {
  private bindings: HotkeyBinding[] = [];
  private held = new Set<string>();
  private active = new Set<string>();

  setBindings(bindings: HotkeyBinding[]): Fired[] {
    const released = this.releaseAll();
    this.bindings = bindings.filter((b) => b.keys.length > 0);
    return released;
  }

  releaseAll(): Fired[] {
    this.held.clear();
    const out: Fired[] = [];
    for (const b of this.bindings) {
      if (this.active.has(b.id)) out.push({ id: b.id, action: b.action, pressed: false });
    }
    this.active.clear();
    return out;
  }

  key(key: string, down: boolean): Fired[] {
    const out: Fired[] = [];
    if (down) {
      if (this.held.has(key)) return out; // auto-repeat
      this.held.add(key);
      const heldMods = this.heldMods();
      for (const b of this.bindings) {
        if (!this.allHeld(b)) continue;
        if (isHoldAction(b.action)) {
          if (!this.active.has(b.id)) {
            this.active.add(b.id);
            out.push({ id: b.id, action: b.action, pressed: true });
          }
        } else if (partMatches(b, key) && sameMods(heldMods, bindingMods(b))) {
          out.push({ id: b.id, action: b.action, pressed: true });
        }
      }
    } else {
      if (!this.held.delete(key)) return out;
      for (const b of this.bindings) {
        if (this.active.has(b.id) && !this.allHeld(b)) {
          this.active.delete(b.id);
          out.push({ id: b.id, action: b.action, pressed: false });
        }
      }
    }
    return out;
  }

  private heldMods(): Set<Modifier> {
    const mods = new Set<Modifier>();
    for (const k of this.held) {
      const m = modifierOf(k);
      if (m) mods.add(m);
    }
    return mods;
  }

  private allHeld(b: HotkeyBinding): boolean {
    return b.keys.every((k) =>
      isGenericModifier(k) ? [...this.held].some((h) => modifierOf(h) === k) : this.held.has(k),
    );
  }
}

function bindingMods(b: HotkeyBinding): Set<Modifier> {
  const mods = new Set<Modifier>();
  for (const k of b.keys) {
    const m = isGenericModifier(k) ? k : modifierOf(k);
    if (m) mods.add(m);
  }
  return mods;
}

function sameMods(a: Set<Modifier>, b: Set<Modifier>): boolean {
  return a.size === b.size && [...a].every((m) => b.has(m));
}

function partMatches(b: HotkeyBinding, key: string): boolean {
  return b.keys.some((k) => (isGenericModifier(k) ? modifierOf(key) === k : k === key));
}
