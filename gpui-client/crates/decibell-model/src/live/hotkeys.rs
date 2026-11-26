//! Global hotkeys and the input mode (`stores/hotkeysStore.ts`,
//! `features/hotkeys/hotkeyRuntime.ts`, `keys.ts`; spec `2026-10-10-global-hotkeys-
//! design.md`; CLAUDE.md "Global hotkeys see bound keys only").
//!
//! The bindings, the input mode and the push-to-talk release delay are `Settings` fields
//! (`hotkeys`, `input_mode`, `ptt_release_delay_ms`): Settings → Keybinds changes them
//! with `Settings::change`. This store keeps the core in step with them (observing
//! `Settings`: `hotkeys_configure`, one in flight at a time with edits made meanwhile
//! collapsed into one follow-up; `set_input_mode`; `hotkeys_set_paused` while the
//! recorder captures a combo), mirrors the listener's status, and runs the **focused
//! fallback**: where the listener misses keys typed into Decibell itself (XWayland, macOS
//! hold actions) the app feeds its own window's key events here ([`Hotkeys::key`]) and
//! the core's own matcher decides what fires.
//!
//! Press actions (mute, deafen, leave, answer, decline) reach the model as
//! `hotkey_action` and are routed in `events::misc`; push to talk / push to mute never
//! come here (the core gates the mic itself).
//!
//! Keys are DOM `KeyboardEvent.code` names (physical, layout-free), generic `Control` /
//! `Shift` / `Alt` / `Meta` in a combo with a real key, sided codes only in modifier-only
//! combos, and `Mouse3`..`Mouse5`; the core speaks the same spelling. GPUI's key events
//! carry the platform's physical key (`KeyCode`, GPUI patch 0012: the evdev code on Linux,
//! the scan code on Windows), and [`code_of`] names it through the core's own table
//! (`hotkeys/keys.rs`), so the [`Recorder`] and the focused fallback
//! ([`Hotkeys::key_event`]) see the same key whatever the keyboard layout types with it.
//! Only where the platform gives no code (macOS at this patch) is the name guessed from
//! the character, as a US layout would produce it ([`code_from_gpui_key`]).

use std::rc::Rc;

use decibell_core::config::{AppSettings, HotkeyBindingSetting};
use decibell_core::hotkeys::matcher::{Fired, Matcher};
use decibell_core::hotkeys::{Action, Binding, Status, keys};
use futures::future::LocalBoxFuture;
use gpui::{Context, Task};

use crate::error::UiError;

pub const DEFAULT_PTT_RELEASE_DELAY_MS: u32 = 100;
pub const MAX_PTT_RELEASE_DELAY_MS: u32 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    VoiceActivity,
    PushToTalk,
}

impl InputMode {
    pub fn of(s: &AppSettings) -> Self {
        if s.input_mode.as_deref() == Some("push_to_talk") { Self::PushToTalk } else { Self::VoiceActivity }
    }

    pub fn as_setting(self) -> &'static str {
        match self {
            Self::VoiceActivity => "voice_activity",
            Self::PushToTalk => "push_to_talk",
        }
    }
}

pub fn ptt_release_delay_ms(s: &AppSettings) -> u32 {
    s.ptt_release_delay_ms.unwrap_or(DEFAULT_PTT_RELEASE_DELAY_MS).min(MAX_PTT_RELEASE_DELAY_MS)
}

/// The actions, in the Keybinds tab's order, with their labels and whether they are held.
pub const HOTKEY_ACTIONS: [(Action, &str, bool); 7] = [
    (Action::PushToTalk, "Push to talk", true),
    (Action::ToggleMute, "Toggle mute", false),
    (Action::ToggleDeafen, "Toggle deafen", false),
    (Action::PushToMute, "Push to mute", true),
    (Action::LeaveVoice, "Leave voice / hang up", false),
    (Action::AnswerCall, "Answer call", false),
    (Action::DeclineCall, "Decline call", false),
];

/// The saved bindings this build understands: an unknown action or an empty combo is
/// dropped, not guessed at (`loadSettings`).
pub fn bindings(s: &AppSettings) -> Vec<Binding> {
    s.hotkeys
        .iter()
        .filter(|b| !b.keys.is_empty())
        .filter_map(|b| Some(Binding { id: b.id.clone(), action: Action::parse(&b.action)?, keys: b.keys.clone() }))
        .collect()
}

/// Write bindings back into the settings blob.
pub fn store_bindings(s: &mut AppSettings, bindings: &[Binding]) {
    s.hotkeys = bindings
        .iter()
        .map(|b| HotkeyBindingSetting {
            id: b.id.clone(),
            action: serde_json::to_value(b.action).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(),
            keys: b.keys.clone(),
        })
        .collect();
}

const MODIFIER_ORDER: [&str; 4] = ["Control", "Alt", "Shift", "Meta"];

/// The modifier a sided key code belongs to.
pub fn modifier_of(code: &str) -> Option<&'static str> {
    match code {
        "ControlLeft" | "ControlRight" => Some("Control"),
        "ShiftLeft" | "ShiftRight" => Some("Shift"),
        "AltLeft" | "AltRight" => Some("Alt"),
        "MetaLeft" | "MetaRight" => Some("Meta"),
        _ => None,
    }
}

fn is_generic_modifier(key: &str) -> bool {
    MODIFIER_ORDER.contains(&key)
}

/// What the recorder saw → a binding's keys (`normalizeCombo`): with a real key the
/// modifiers go generic (either side); a modifier-only combo keeps its sides.
pub fn normalize_combo(physical: &[String]) -> Vec<String> {
    let mains: Vec<&String> = physical.iter().filter(|k| modifier_of(k).is_none()).collect();
    if mains.is_empty() {
        let mut keys: Vec<String> = physical.to_vec();
        keys.sort();
        keys.dedup();
        let order = |k: &String| MODIFIER_ORDER.iter().position(|m| Some(*m) == modifier_of(k)).unwrap_or(4);
        keys.sort_by(|a, b| order(a).cmp(&order(b)).then_with(|| a.cmp(b)));
        return keys;
    }
    let mut out: Vec<String> = MODIFIER_ORDER
        .iter()
        .filter(|m| physical.iter().any(|k| modifier_of(k) == Some(**m)))
        .map(|m| m.to_string())
        .collect();
    out.push(mains[mains.len() - 1].clone());
    out
}

pub fn same_combo(a: &[String], b: &[String]) -> bool {
    a.len() == b.len() && a.iter().all(|k| b.contains(k))
}

/// The desktop's key grab (the portal) can only express modifiers + one keyboard key.
pub fn portal_can_bind(keys: &[String]) -> bool {
    !keys.iter().any(|k| keys::is_mouse(k)) && keys.iter().any(|k| !is_generic_modifier(k) && modifier_of(k).is_none())
}

/// A known key name (validation for the recorder and the config).
pub fn is_known_key(key: &str) -> bool {
    keys::intern(key).is_some()
}

/// A mouse button (GPUI's `MouseButton`) → its key name; left / right can't be bound.
pub fn mouse_key(button: gpui::MouseButton) -> Option<&'static str> {
    match button {
        gpui::MouseButton::Middle => Some("Mouse3"),
        gpui::MouseButton::Navigate(gpui::NavigationDirection::Back) => Some("Mouse4"),
        gpui::MouseButton::Navigate(gpui::NavigationDirection::Forward) => Some("Mouse5"),
        _ => None,
    }
}

/// Every `KeyboardEvent.code` the core's table has (`decibell_core::hotkeys::keys`). Only
/// the names live here: the numbers come from the core (`evdev_code`, `win_scancode`),
/// which has no reverse lookup. A test checks every name is one the core knows.
const CODE_NAMES: [&str; 177] = [
    "KeyA", "KeyB", "KeyC", "KeyD", "KeyE", "KeyF", "KeyG", "KeyH", "KeyI", "KeyJ", "KeyK", "KeyL", "KeyM", "KeyN",
    "KeyO", "KeyP", "KeyQ", "KeyR", "KeyS", "KeyT", "KeyU", "KeyV", "KeyW", "KeyX", "KeyY", "KeyZ", "Digit1",
    "Digit2", "Digit3", "Digit4", "Digit5", "Digit6", "Digit7", "Digit8", "Digit9", "Digit0", "Enter", "Escape",
    "Backspace", "Tab", "Space", "Minus", "Equal", "BracketLeft", "BracketRight", "Backslash", "Semicolon", "Quote",
    "Backquote", "Comma", "Period", "Slash", "CapsLock", "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9",
    "F10", "F11", "F12", "PrintScreen", "ScrollLock", "Pause", "Insert", "Home", "PageUp", "Delete", "End",
    "PageDown", "ArrowRight", "ArrowLeft", "ArrowDown", "ArrowUp", "NumLock", "NumpadDivide", "NumpadMultiply",
    "NumpadSubtract", "NumpadAdd", "NumpadEnter", "Numpad1", "Numpad2", "Numpad3", "Numpad4", "Numpad5", "Numpad6",
    "Numpad7", "Numpad8", "Numpad9", "Numpad0", "NumpadDecimal", "IntlBackslash", "ContextMenu", "Power",
    "NumpadEqual", "F13", "F14", "F15", "F16", "F17", "F18", "F19", "F20", "F21", "F22", "F23", "F24", "Open",
    "Help", "Select", "Again", "Undo", "Cut", "Copy", "Paste", "Find", "AudioVolumeMute", "AudioVolumeUp",
    "AudioVolumeDown", "NumpadComma", "IntlRo", "KanaMode", "IntlYen", "Convert", "NonConvert", "Lang1", "Lang2",
    "Lang3", "Lang4", "Lang5", "NumpadParenLeft", "NumpadParenRight", "ControlLeft", "ShiftLeft", "AltLeft",
    "MetaLeft", "ControlRight", "ShiftRight", "AltRight", "MetaRight", "BrightnessUp", "BrightnessDown",
    "MediaPlay", "MediaPause", "MediaRecord", "MediaFastForward", "MediaRewind", "MediaTrackNext",
    "MediaTrackPrevious", "MediaStop", "Eject", "MediaPlayPause", "MediaSelect", "LaunchMail", "LaunchApp2",
    "LaunchApp1", "LaunchControlPanel", "SelectTask", "LaunchScreenSaver", "LaunchAssistant", "BrowserSearch",
    "BrowserHome", "BrowserBack", "BrowserForward", "BrowserStop", "BrowserRefresh", "BrowserFavorites",
    "ZoomToggle", "MailReply", "MailForward", "MailSend", "KeyboardLayoutSelect", "ShowAllWindows",
];

/// The name (`KeyboardEvent.code`) of a physical key as GPUI reports it. `None` for a key
/// the core's table lacks and for [`gpui::KeyCode::Unknown`].
pub fn code_of(code: gpui::KeyCode) -> Option<&'static str> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    type Table = HashMap<u16, &'static str>;
    static EVDEV: OnceLock<Table> = OnceLock::new();
    static WINDOWS: OnceLock<Table> = OnceLock::new();
    let table = |number: fn(&str) -> Option<u16>| -> Table {
        // The first name wins where two share a number, as in the core's own lookups.
        let mut table = Table::new();
        for name in CODE_NAMES {
            if let Some(number) = number(name) {
                table.entry(number).or_insert(name);
            }
        }
        table
    };
    match code {
        gpui::KeyCode::Evdev(code) => EVDEV.get_or_init(|| table(keys::evdev_code)).get(&code).copied(),
        gpui::KeyCode::Windows(code) => WINDOWS.get_or_init(|| table(keys::win_scancode)).get(&code).copied(),
        gpui::KeyCode::Unknown => None,
    }
}

/// The key a key event is about: its physical code, or, where the platform reports none,
/// the US-layout guess from the character.
pub fn event_code(code: gpui::KeyCode, keystroke: &gpui::Keystroke) -> Option<String> {
    match code {
        gpui::KeyCode::Unknown => code_from_gpui_key(&keystroke.key),
        code => code_of(code).map(str::to_string),
    }
}

/// What the Keybinds tab's recorder decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// The combo to bind (already normalised: [`normalize_combo`]).
    Combo(Vec<String>),
    /// Escape with nothing held: recording is cancelled.
    Cancelled,
}

/// The Keybinds tab's recorder (`KeybindsTab.tsx` `startRecording`): feed it the window's
/// key and mouse events while it records; it answers when the combo is complete. A
/// non-modifier key completes the combo with the modifiers held; modifiers alone complete
/// when the last one is released (and keep their sides); Mouse3..5 complete with the
/// modifiers held; auto-repeats are ignored.
#[derive(Debug, Default)]
pub struct Recorder {
    held: Vec<String>,
    peak: Vec<String>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    /// A key went down, by name (`KeyboardEvent.code`).
    pub fn key_down(&mut self, code: &str, repeat: bool) -> Option<Recorded> {
        if repeat {
            return None;
        }
        if code == "Escape" && self.held.is_empty() {
            return Some(Recorded::Cancelled);
        }
        for keys in [&mut self.held, &mut self.peak] {
            if !keys.iter().any(|key| key == code) {
                keys.push(code.to_string());
            }
        }
        modifier_of(code).is_none().then(|| Recorded::Combo(normalize_combo(&self.held)))
    }

    pub fn key_up(&mut self, code: &str) -> Option<Recorded> {
        self.held.retain(|key| key != code);
        (self.held.is_empty() && !self.peak.is_empty()).then(|| Recorded::Combo(normalize_combo(&self.peak)))
    }

    /// GPUI's key-down event.
    pub fn on_key_down(&mut self, event: &gpui::KeyDownEvent) -> Option<Recorded> {
        let code = event_code(event.code, &event.keystroke)?;
        self.key_down(&code, event.is_held)
    }

    pub fn on_key_up(&mut self, event: &gpui::KeyUpEvent) -> Option<Recorded> {
        let code = event_code(event.code, &event.keystroke)?;
        self.key_up(&code)
    }

    /// GPUI's modifiers-changed event: a modifier key going down or up, when it says which
    /// one (`ModifiersChangedEvent::key`, patch 0012).
    pub fn on_modifiers_changed(&mut self, event: &gpui::ModifiersChangedEvent) -> Option<Recorded> {
        let key = event.key?;
        let code = code_of(key.code)?;
        if key.pressed { self.key_down(code, false) } else { self.key_up(code) }
    }

    /// A mouse button: Mouse3..5 complete the combo with the modifiers held.
    pub fn on_mouse_down(&mut self, button: gpui::MouseButton) -> Option<Recorded> {
        let key = mouse_key(button)?;
        let mut keys: Vec<String> = self.held.iter().filter(|held| modifier_of(held).is_some()).cloned().collect();
        keys.push(key.to_string());
        Some(Recorded::Combo(normalize_combo(&keys)))
    }
}

/// GPUI's `Keystroke::key` → a `KeyboardEvent.code`, **assuming a US layout**: only for a
/// platform that reports no physical key (see [`event_code`]).
pub fn code_from_gpui_key(key: &str) -> Option<String> {
    let code = match key {
        k if k.len() == 1 && k.as_bytes()[0].is_ascii_lowercase() => format!("Key{}", k.to_ascii_uppercase()),
        k if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() => format!("Digit{k}"),
        "space" => "Space".into(),
        "enter" => "Enter".into(),
        "escape" => "Escape".into(),
        "backspace" => "Backspace".into(),
        "tab" => "Tab".into(),
        "insert" => "Insert".into(),
        "delete" => "Delete".into(),
        "home" => "Home".into(),
        "end" => "End".into(),
        "pageup" => "PageUp".into(),
        "pagedown" => "PageDown".into(),
        "up" => "ArrowUp".into(),
        "down" => "ArrowDown".into(),
        "left" => "ArrowLeft".into(),
        "right" => "ArrowRight".into(),
        "-" => "Minus".into(),
        "=" => "Equal".into(),
        "[" => "BracketLeft".into(),
        "]" => "BracketRight".into(),
        "\\" => "Backslash".into(),
        ";" => "Semicolon".into(),
        "'" => "Quote".into(),
        "`" => "Backquote".into(),
        "," => "Comma".into(),
        "." => "Period".into(),
        "/" => "Slash".into(),
        k if k.starts_with('f') && k[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => k.to_uppercase(),
        _ => return None,
    };
    is_known_key(&code).then_some(code)
}

/// A key's label in the UI (`keyLabel`, without the layout map Chromium had: letters,
/// digits and US punctuation).
pub fn key_label(key: &str) -> String {
    let platform_meta = if cfg!(target_os = "macos") { "Cmd" } else if cfg!(target_os = "windows") { "Win" } else { "Super" };
    let modifier_label = |m: &str| match m {
        "Control" => "Ctrl",
        "Shift" => "Shift",
        "Alt" => if cfg!(target_os = "macos") { "Option" } else { "Alt" },
        _ => platform_meta,
    };
    if is_generic_modifier(key) {
        return modifier_label(key).into();
    }
    if let Some(m) = modifier_of(key) {
        let side = if key.ends_with("Right") { "Right" } else { "Left" };
        return format!("{side} {}", modifier_label(m));
    }
    let named = match key {
        "Space" => "Space",
        "Enter" => "Enter",
        "Escape" => "Esc",
        "Backspace" => "Backspace",
        "Tab" => "Tab",
        "CapsLock" => "Caps Lock",
        "Insert" => "Insert",
        "Delete" => "Delete",
        "Home" => "Home",
        "End" => "End",
        "PageUp" => "Page Up",
        "PageDown" => "Page Down",
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        "ArrowLeft" => "←",
        "ArrowRight" => "→",
        "PrintScreen" => "Print Screen",
        "ScrollLock" => "Scroll Lock",
        "Pause" => "Pause",
        "NumLock" => "Num Lock",
        "ContextMenu" => "Menu",
        "NumpadAdd" => "Num +",
        "NumpadSubtract" => "Num −",
        "NumpadMultiply" => "Num ×",
        "NumpadDivide" => "Num ÷",
        "NumpadDecimal" => "Num .",
        "NumpadEnter" => "Num Enter",
        "MediaPlayPause" => "Play/Pause",
        "MediaStop" => "Stop",
        "MediaTrackNext" => "Next Track",
        "MediaTrackPrevious" => "Previous Track",
        "AudioVolumeMute" => "Volume Mute",
        "AudioVolumeUp" => "Volume Up",
        "AudioVolumeDown" => "Volume Down",
        "Mouse3" => "Middle Mouse",
        "Mouse4" => "Mouse 4",
        "Mouse5" => "Mouse 5",
        "Minus" => "-",
        "Equal" => "=",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        "Backslash" => "\\",
        "Semicolon" => ";",
        "Quote" => "'",
        "Backquote" => "`",
        "Comma" => ",",
        "Period" => ".",
        "Slash" => "/",
        "IntlBackslash" => "<",
        _ => "",
    };
    if !named.is_empty() {
        return named.into();
    }
    if let Some(rest) = key.strip_prefix("Key").filter(|r| r.len() == 1) {
        return rest.into();
    }
    if let Some(rest) = key.strip_prefix("Digit").filter(|r| r.len() == 1) {
        return rest.into();
    }
    if let Some(rest) = key.strip_prefix("Numpad").filter(|r| r.len() == 1 && r.as_bytes()[0].is_ascii_digit()) {
        return format!("Num {rest}");
    }
    key.into()
}

pub fn combo_label(keys: &[String]) -> String {
    keys.iter().map(|k| key_label(k)).collect::<Vec<_>>().join(" + ")
}

/// `hotkeys_configure`.
pub type Configure = Rc<dyn Fn(Vec<Binding>) -> LocalBoxFuture<'static, Result<(), UiError>>>;
/// `hotkeys_inject`: action, pressed, source (a binding id).
pub type Inject = Rc<dyn Fn(Action, bool, &str)>;

/// How the store reaches the core (`api::hotkeys` in the app, a recorder in tests).
pub struct HotkeyBackend {
    pub configure: Configure,
    pub set_input_mode: Rc<dyn Fn(bool, u32)>,
    pub set_paused: Rc<dyn Fn(bool)>,
    /// A press / release from the focused fallback.
    pub inject: Inject,
}

pub struct Hotkeys {
    /// The listener's status (`hotkeys_status`, or the first `hotkeys_get_status`).
    pub status: Option<Status>,
    /// The Keybinds tab is capturing a combo: the core swallows presses meanwhile.
    pub recording: bool,
    backend: HotkeyBackend,
    /// The newest bindings asked for (sent, in flight or waiting).
    latest: Option<Vec<Binding>>,
    /// The newest bindings, waiting for the configure in flight.
    wanted: Option<Vec<Binding>>,
    configuring: Option<Task<()>>,
    mode_pushed: Option<(bool, u32)>,
    /// The focused fallback: armed bindings (none = off) and the matcher.
    fallback: Option<Vec<Binding>>,
    matcher: Matcher,
    /// The keys the matcher holds (fed down and not yet up), mirrored so [`Hotkeys::would_take`]
    /// can answer without feeding it.
    held: std::collections::HashSet<&'static str>,
}

impl Hotkeys {
    pub fn new(backend: HotkeyBackend) -> Self {
        Hotkeys {
            status: None,
            recording: false,
            backend,
            latest: None,
            wanted: None,
            configuring: None,
            mode_pushed: None,
            fallback: None,
            matcher: Matcher::default(),
            held: Default::default(),
        }
    }

    pub fn configuring(&self) -> bool {
        self.configuring.is_some()
    }

    pub fn fallback_armed(&self) -> bool {
        self.fallback.is_some()
    }

    /// Puts `backend` in place of the core's and returns the one it replaced: the app's
    /// focused-fallback self-test counts what would reach the core, then puts it back.
    pub fn swap_backend(&mut self, backend: HotkeyBackend) -> HotkeyBackend {
        std::mem::replace(&mut self.backend, backend)
    }

    /// Keep the core in step with the settings (called when `Settings` changed).
    pub fn sync(&mut self, settings: &AppSettings, cx: &mut Context<Self>) {
        let bindings = bindings(settings);
        if self.latest.as_ref() != Some(&bindings) {
            self.latest = Some(bindings.clone());
            self.wanted = Some(bindings);
            self.push(cx);
        }
        let mode = (InputMode::of(settings) == InputMode::PushToTalk, ptt_release_delay_ms(settings));
        if self.mode_pushed != Some(mode) {
            self.mode_pushed = Some(mode);
            (self.backend.set_input_mode)(mode.0, mode.1);
        }
        self.sync_fallback(settings);
    }

    /// One configure in flight; what changed meanwhile goes in one follow-up.
    fn push(&mut self, cx: &mut Context<Self>) {
        if self.configuring.is_some() {
            return;
        }
        let Some(bindings) = self.wanted.take() else { return };
        let configure = (self.backend.configure)(bindings);
        self.configuring = Some(cx.spawn(async move |this, cx| {
            if let Err(e) = configure.await {
                log::error!("[hotkeys] configure failed: {e}");
            }
            let _ = this.update(cx, |this, cx| {
                this.configuring = None;
                this.push(cx);
            });
        }));
    }

    pub fn set_status(&mut self, status: Status, settings: &AppSettings, cx: &mut Context<Self>) {
        self.status = Some(status);
        self.sync_fallback(settings);
        cx.notify();
    }

    /// The recorder starts / stops capturing a combo.
    pub fn set_recording(&mut self, recording: bool, settings: &AppSettings, cx: &mut Context<Self>) {
        if self.recording != recording {
            self.recording = recording;
            (self.backend.set_paused)(recording);
            self.sync_fallback(settings);
            cx.notify();
        }
    }

    fn sync_fallback(&mut self, settings: &AppSettings) {
        let on = self.status.as_ref().is_some_and(|s| s.focused_fallback) && !self.recording;
        // macOS: accelerators deliver press actions everywhere, the fallback only holds.
        let hold_only = self.status.as_ref().is_some_and(|s| s.backend == "electron");
        let next = on.then(|| {
            bindings(settings).into_iter().filter(|b| !hold_only || b.action.is_hold()).collect::<Vec<_>>()
        });
        // Re-arming releases whatever is held, so only on a real change.
        if next == self.fallback {
            return;
        }
        let released = self.matcher.set_bindings(next.as_deref().unwrap_or(&[]));
        self.held.clear();
        self.fallback = next;
        self.inject(released);
    }

    fn inject(&self, fired: Vec<Fired>) {
        for f in fired {
            (self.backend.inject)(f.action, f.pressed, &f.id);
        }
    }

    /// A key (`KeyboardEvent.code` name) went down / up in our own window, or a mouse
    /// button (`Mouse3`..`5`). Matched only while the fallback is armed.
    pub fn key(&mut self, code: &str, down: bool) {
        if self.fallback.is_none() {
            return;
        }
        let Some(key) = keys::intern(code) else { return };
        if down {
            self.held.insert(key);
        } else {
            self.held.remove(key);
        }
        let fired = self.matcher.key(key, down);
        self.inject(fired);
    }

    /// Would the focused fallback take this key down (`KeyDownEvent::code` / `keystroke`), with
    /// what it holds now? True when the key, together with the keys held, completes an armed
    /// binding it is part of: every key of a hold binding held (extra keys are fine, as in the
    /// matcher), or a press binding whose keys are held with exactly its modifiers. An
    /// auto-repeat of such a key answers the same. Nothing is fed or changed: "type anywhere to
    /// compose" asks this so that a key the fallback presses (a bare-key push to talk) isn't
    /// typed too (`docs/electron-quirks.md` #17). False while the fallback is off.
    pub fn would_take(&self, code: gpui::KeyCode, keystroke: &gpui::Keystroke) -> bool {
        let Some(bindings) = self.fallback.as_deref() else { return false };
        let Some(key) = event_code(code, keystroke).and_then(|name| keys::intern(&name)) else { return false };
        would_complete(bindings, &self.held, key)
    }

    /// The focused fallback fed from GPUI's events: a key of our own window went down or
    /// up, named by its physical code (so a binding fires on the key it was recorded on,
    /// whatever the layout). Call it with `KeyDownEvent` / `KeyUpEvent` (`code`,
    /// `keystroke`) and with `ModifiersChangedEvent::key` for the modifier keys.
    pub fn key_event(&mut self, code: gpui::KeyCode, keystroke: Option<&gpui::Keystroke>, down: bool) {
        if self.fallback.is_none() {
            return;
        }
        let name = match keystroke {
            Some(keystroke) => event_code(code, keystroke),
            None => code_of(code).map(str::to_string),
        };
        if let Some(name) = name {
            self.key(&name, down);
        }
    }

    /// Our window lost focus: keys released meanwhile never reach us.
    pub fn window_blurred(&mut self) {
        if self.fallback.is_some() {
            self.held.clear();
            let fired = self.matcher.release_all();
            self.inject(fired);
        }
    }
}

/// Whether `key` going down while `held` is held completes one of `bindings` that it is part
/// of, by the core matcher's rules (`hotkeys/matcher.rs`): a generic modifier (`Control`)
/// matches either side; a hold binding wants all its keys held, extra keys allowed; a press
/// binding also wants exactly its modifiers held.
fn would_complete(bindings: &[Binding], held: &std::collections::HashSet<&'static str>, key: &'static str) -> bool {
    let is_held = |k: &str| k == key || held.contains(k);
    let mut held_mods = keys::Mods::default();
    for k in held.iter().copied().chain([key]) {
        if let Some(m) = keys::sided_modifier(k) {
            held_mods.insert(m);
        }
    }
    bindings.iter().any(|binding| {
        let mut part_of = false;
        let mut mods = keys::Mods::default();
        let mut complete = !binding.keys.is_empty();
        for k in &binding.keys {
            let Some(k) = keys::intern(k) else { return false };
            if let Some(m) = keys::generic_modifier(k) {
                mods.insert(m);
                part_of |= keys::sided_modifier(key) == Some(m);
                complete &= keys::sided_modifier(key) == Some(m) || held.iter().any(|h| keys::sided_modifier(h) == Some(m));
            } else {
                if let Some(m) = keys::sided_modifier(k) {
                    mods.insert(m);
                }
                part_of |= k == key;
                complete &= is_held(k);
            }
        }
        part_of && complete && (binding.action.is_hold() || held_mods == mods)
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    #[derive(Default)]
    struct Calls {
        configures: Vec<Vec<String>>,
        modes: Vec<(bool, u32)>,
        paused: Vec<bool>,
        injected: Vec<(Action, bool, String)>,
    }

    fn backend(calls: Rc<RefCell<Calls>>, executor: gpui::BackgroundExecutor) -> HotkeyBackend {
        let (c1, c2, c3, c4) = (calls.clone(), calls.clone(), calls.clone(), calls);
        HotkeyBackend {
            configure: Rc::new(move |b| {
                c1.borrow_mut().configures.push(b.iter().map(|b| b.id.clone()).collect());
                let executor = executor.clone();
                Box::pin(async move {
                    executor.timer(std::time::Duration::from_millis(100)).await;
                    Ok(())
                })
            }),
            set_input_mode: Rc::new(move |ptt, delay| c2.borrow_mut().modes.push((ptt, delay))),
            set_paused: Rc::new(move |p| c3.borrow_mut().paused.push(p)),
            inject: Rc::new(move |a, p, s| c4.borrow_mut().injected.push((a, p, s.to_string()))),
        }
    }

    fn binding(id: &str, action: &str, keys: &[&str]) -> HotkeyBindingSetting {
        HotkeyBindingSetting { id: id.into(), action: action.into(), keys: keys.iter().map(|k| k.to_string()).collect() }
    }

    fn status(fallback: bool) -> Status {
        let mut s = decibell_core::hotkeys::status();
        s.focused_fallback = fallback;
        s
    }

    /// Edits made while a configure is in flight collapse into one follow-up; unknown
    /// actions and empty combos never reach the core; the input mode is pushed on change.
    #[gpui::test]
    fn bindings_and_mode_follow_the_settings(cx: &mut TestAppContext) {
        let calls: Rc<RefCell<Calls>> = Rc::default();
        let executor = cx.executor();
        let h = cx.new(|_| Hotkeys::new(backend(calls.clone(), executor)));
        let mut s = AppSettings {
            hotkeys: vec![binding("a", "toggle_mute", &["Control", "KeyM"]), binding("x", "fly", &["KeyF"]), binding("e", "toggle_deafen", &[])],
            ..AppSettings::default()
        };
        h.update(cx, |h, cx| h.sync(&s, cx));
        s.hotkeys.push(binding("b", "push_to_talk", &["Mouse4"]));
        h.update(cx, |h, cx| h.sync(&s, cx));
        s.hotkeys.push(binding("c", "leave_voice", &["F9"]));
        s.input_mode = Some("push_to_talk".into());
        s.ptt_release_delay_ms = Some(5000);
        h.update(cx, |h, cx| h.sync(&s, cx));
        cx.executor().advance_clock(std::time::Duration::from_millis(250));
        cx.run_until_parked();
        let calls = calls.borrow();
        assert_eq!(calls.configures, vec![vec!["a".to_string()], vec!["a".into(), "b".into(), "c".into()]]);
        assert_eq!(calls.modes, vec![(false, 100), (true, 1000)]);
        assert!(!h.read_with(cx, |h, _| h.configuring()));
    }

    /// The focused fallback: armed only while the listener asks for it and nobody is
    /// recording; matched keys inject with the binding id as the source.
    #[gpui::test]
    fn the_focused_fallback_matches_window_keys(cx: &mut TestAppContext) {
        let calls: Rc<RefCell<Calls>> = Rc::default();
        let executor = cx.executor();
        let h = cx.new(|_| Hotkeys::new(backend(calls.clone(), executor)));
        let s = AppSettings {
            hotkeys: vec![binding("m", "toggle_mute", &["Control", "KeyM"]), binding("p", "push_to_talk", &["KeyV"])],
            ..AppSettings::default()
        };
        h.update(cx, |h, cx| {
            h.sync(&s, cx);
            h.key("KeyV", true);
        });
        assert!(calls.borrow().injected.is_empty(), "not armed without the listener's request");
        h.update(cx, |h, cx| h.set_status(status(true), &s, cx));
        h.update(cx, |h, _| {
            h.key("ControlLeft", true);
            h.key("KeyM", true);
            h.key("KeyM", true); // auto-repeat
            h.key("KeyV", true);
        });
        assert_eq!(
            calls.borrow().injected,
            vec![(Action::ToggleMute, true, "m".into()), (Action::PushToTalk, true, "p".into())]
        );
        // Recording pauses the core and releases what is held.
        h.update(cx, |h, cx| h.set_recording(true, &s, cx));
        assert_eq!(calls.borrow().paused, vec![true]);
        assert_eq!(calls.borrow().injected.last().unwrap(), &(Action::PushToTalk, false, "p".into()));
        assert!(!h.read_with(cx, |h, _| h.fallback_armed()));
    }

    /// Our window going to the background releases what the fallback holds (the key's release
    /// never reaches us), once; not armed, nothing is injected.
    #[gpui::test]
    fn a_blurred_window_releases_held_keys(cx: &mut TestAppContext) {
        let calls: Rc<RefCell<Calls>> = Rc::default();
        let executor = cx.executor();
        let h = cx.new(|_| Hotkeys::new(backend(calls.clone(), executor)));
        let s = AppSettings { hotkeys: vec![binding("p", "push_to_talk", &["KeyV"])], ..AppSettings::default() };
        h.update(cx, |h, cx| {
            h.sync(&s, cx);
            h.window_blurred();
        });
        assert!(calls.borrow().injected.is_empty(), "not armed");
        h.update(cx, |h, cx| {
            h.set_status(status(true), &s, cx);
            h.key("KeyV", true);
            h.window_blurred();
            h.window_blurred();
            h.key("KeyV", false);
        });
        assert_eq!(
            calls.borrow().injected,
            vec![(Action::PushToTalk, true, "p".into()), (Action::PushToTalk, false, "p".into())]
        );
    }

    /// "Would the fallback take this key" answers as the matcher would fire, without feeding
    /// it: a bare-key push to talk, its auto-repeat, a press combo only with exactly its
    /// modifiers, a two-key hold only with the other key held; nothing while not armed.
    #[gpui::test]
    fn would_take_answers_without_feeding(cx: &mut TestAppContext) {
        let calls: Rc<RefCell<Calls>> = Rc::default();
        let executor = cx.executor();
        let h = cx.new(|_| Hotkeys::new(backend(calls.clone(), executor)));
        let s = AppSettings {
            hotkeys: vec![
                binding("p", "push_to_talk", &["KeyV"]),
                binding("m", "toggle_mute", &["Shift", "KeyM"]),
                binding("d", "toggle_deafen", &["Control", "Semicolon"]),
                binding("q", "push_to_mute", &["KeyQ", "KeyW"]),
            ],
            ..AppSettings::default()
        };
        fn key(name: &'static str) -> gpui::Keystroke {
            gpui::Keystroke::parse(name).unwrap()
        }
        let ev = |code: u16| gpui::KeyCode::Evdev(code);
        h.update(cx, |h, cx| h.sync(&s, cx));
        assert!(!h.read_with(cx, |h, _| h.would_take(ev(47), &key("v"))), "not armed");
        h.update(cx, |h, cx| h.set_status(status(true), &s, cx));
        let (v, a, m, semi, q) = h.read_with(cx, |h, _| {
            (
                h.would_take(ev(47), &key("v")),
                h.would_take(ev(30), &key("a")),
                h.would_take(ev(50), &key("m")),
                h.would_take(ev(39), &key(";")),
                h.would_take(ev(16), &key("q")),
            )
        });
        assert!(v && !a && !m && !semi && !q, "bare: v {v} a {a} m {m} ; {semi} q {q}");
        // Shift held: Shift+M is a press combo with exactly its modifiers; V still talks.
        h.update(cx, |h, _| h.key("ShiftLeft", true));
        let (m, v) = h.read_with(cx, |h, _| (h.would_take(ev(50), &key("shift-m")), h.would_take(ev(47), &key("shift-v"))));
        assert!(m && v, "with Shift: m {m} v {v}");
        h.update(cx, |h, _| h.key("ShiftLeft", false));
        // W held: Q completes the two-key hold. V held: its repeat is still the fallback's.
        h.update(cx, |h, _| {
            h.key("KeyW", true);
            h.key("KeyV", true);
        });
        let (q, v) = h.read_with(cx, |h, _| (h.would_take(ev(16), &key("q")), h.would_take(ev(47), &key("v"))));
        assert!(q && v, "W held: q {q}; V held: v {v}");
        let injected = calls.borrow().injected.len();
        h.update(cx, |h, _| h.window_blurred());
        assert!(!h.read_with(cx, |h, _| h.would_take(ev(16), &key("q"))), "released on blur");
        // Asking fed nothing: only the real presses (V) and the blur's release were injected.
        assert_eq!(injected, 1);
    }

    /// The names here are the core's: every one is in its table, the numbers round-trip,
    /// and the Windows spelling (set-1 scan code, `0xE0xx` for extended keys) maps too.
    #[test]
    fn physical_codes_are_named_by_the_cores_table() {
        for name in CODE_NAMES {
            assert_eq!(keys::intern(name), Some(name), "{name} is not in the core's table");
            if let Some(evdev) = keys::evdev_code(name) {
                let back = code_of(gpui::KeyCode::Evdev(evdev)).unwrap();
                assert_eq!(keys::evdev_code(back), Some(evdev), "{name}");
            }
            if let Some(scan) = keys::win_scancode(name) {
                let back = code_of(gpui::KeyCode::Windows(scan)).unwrap();
                assert_eq!(keys::win_scancode(back), Some(scan), "{name}");
            }
        }
        let evdev = |code| code_of(gpui::KeyCode::Evdev(code));
        assert_eq!(evdev(39), Some("Semicolon"));
        assert_eq!(evdev(30), Some("KeyA"));
        assert_eq!(evdev(97), Some("ControlRight"));
        assert_eq!(evdev(0), None);
        let windows = |code| code_of(gpui::KeyCode::Windows(code));
        assert_eq!(windows(0x0027), Some("Semicolon"));
        assert_eq!(windows(0x001d), Some("ControlLeft"));
        assert_eq!(windows(0xe01d), Some("ControlRight"));
        assert_eq!(windows(0x0036), Some("ShiftRight"));
        assert_eq!(windows(0xe048), Some("ArrowUp"));
        assert_eq!(windows(0x0048), Some("Numpad8"));
        assert_eq!(code_of(gpui::KeyCode::Unknown), None);
        // Without a code (macOS at this patch) the character is all there is.
        let keystroke = gpui::Keystroke::parse("m").unwrap();
        assert_eq!(event_code(gpui::KeyCode::Unknown, &keystroke).as_deref(), Some("KeyM"));
        assert_eq!(event_code(gpui::KeyCode::Evdev(39), &keystroke).as_deref(), Some("Semicolon"));
    }

    /// Key events as they arrive under a non-US layout: xkbcommon compiles the layout and
    /// says what each physical key types; the event carries that character and the key's
    /// evdev code, as GPUI's Linux layer builds it (patch 0012).
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    mod layouts {
        use xkbcommon::xkb;

        use super::super::*;

        pub struct Layout(xkb::State);

        impl Layout {
            pub fn new(layout: &str) -> Self {
                let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
                let keymap = xkb::Keymap::new_from_names(&context, "evdev", "pc105", layout, "", None, xkb::KEYMAP_COMPILE_NO_FLAGS)
                    .unwrap_or_else(|| panic!("no xkb layout {layout:?} (xkeyboard-config)"));
                Self(xkb::State::new(&keymap))
            }

            /// What the key with this evdev code types (X11 keycode = evdev + 8).
            pub fn typed(&self, evdev: u16) -> String {
                self.0.key_get_utf8(xkb::Keycode::new(evdev as u32 + 8))
            }

            pub fn down(&self, evdev: u16) -> gpui::KeyDownEvent {
                gpui::KeyDownEvent {
                    keystroke: self.keystroke(evdev),
                    is_held: false,
                    prefer_character_input: false,
                    code: gpui::KeyCode::Evdev(evdev),
                }
            }

            pub fn up(&self, evdev: u16) -> gpui::KeyUpEvent {
                gpui::KeyUpEvent {
                    keystroke: self.keystroke(evdev),
                    code: gpui::KeyCode::Evdev(evdev),
                }
            }

            fn keystroke(&self, evdev: u16) -> gpui::Keystroke {
                let typed = self.typed(evdev);
                gpui::Keystroke {
                    modifiers: gpui::Modifiers::default(),
                    key: typed.clone(),
                    key_char: Some(typed),
                }
            }
        }

        pub fn modifier(evdev: u16, pressed: bool) -> gpui::ModifiersChangedEvent {
            gpui::ModifiersChangedEvent {
                key: Some(gpui::ModifierKey {
                    code: gpui::KeyCode::Evdev(evdev),
                    pressed,
                }),
                ..Default::default()
            }
        }

        fn combo(keys: &[&str]) -> Option<Recorded> {
            Some(Recorded::Combo(keys.iter().map(|key| key.to_string()).collect()))
        }

        /// (evdev code, what the layout types with it, the key's name).
        const TURKISH_Q: [(u16, &str, &str); 9] = [
            (39, "ş", "Semicolon"),
            (23, "ı", "KeyI"),
            (40, "i", "Quote"),
            (26, "ğ", "BracketLeft"),
            (27, "ü", "BracketRight"),
            (51, "ö", "Comma"),
            (52, "ç", "Period"),
            (16, "q", "KeyQ"),
            (41, "\"", "Backquote"),
        ];
        const GERMAN: [(u16, &str, &str); 7] = [
            (21, "z", "KeyY"),
            (44, "y", "KeyZ"),
            (26, "ü", "BracketLeft"),
            (39, "ö", "Semicolon"),
            (40, "ä", "Quote"),
            (12, "ß", "Minus"),
            (53, "-", "Slash"),
        ];

        #[test]
        fn the_recorder_stores_the_physical_key_under_turkish_and_german_layouts() {
            for (name, keys) in [("tr", &TURKISH_Q[..]), ("de", &GERMAN[..])] {
                let layout = Layout::new(name);
                for &(evdev, typed, code) in keys {
                    assert_eq!(layout.typed(evdev), typed, "{name}: what evdev {evdev} types");
                    let mut recorder = Recorder::new();
                    assert_eq!(recorder.on_key_down(&layout.down(evdev)), combo(&[code]), "{name}: the key that types {typed:?}");
                }
            }
            // What the character alone would have given (the recorder before patch 0012):
            // nothing for ş, ğ, ü, ö, ç, ß, and the wrong key for the letters that moved.
            assert_eq!(code_from_gpui_key("ş"), None);
            assert_eq!(code_from_gpui_key("i").as_deref(), Some("KeyI"), "Turkish i is on the Quote key");
            assert_eq!(code_from_gpui_key("z").as_deref(), Some("KeyZ"), "German z is on the KeyY key");
        }

        #[test]
        fn combos_with_modifiers_keep_the_physical_key_and_the_modifier_sides() {
            let layout = Layout::new("tr");
            // Left Ctrl + the ş key: generic Control + Semicolon.
            let mut recorder = Recorder::new();
            assert_eq!(recorder.on_modifiers_changed(&modifier(29, true)), None);
            assert_eq!(recorder.on_key_down(&layout.down(39)), combo(&["Control", "Semicolon"]));
            // Modifiers alone: complete when the last one is released, sides kept.
            let mut recorder = Recorder::new();
            assert_eq!(recorder.on_modifiers_changed(&modifier(54, true)), None); // right Shift
            assert_eq!(recorder.on_modifiers_changed(&modifier(29, true)), None); // left Ctrl
            assert_eq!(recorder.on_modifiers_changed(&modifier(54, false)), None);
            assert_eq!(recorder.on_modifiers_changed(&modifier(29, false)), combo(&["ControlLeft", "ShiftRight"]));
            // A modifiers event that names no key (focus, a layout switch) records nothing.
            let mut recorder = Recorder::new();
            assert_eq!(recorder.on_modifiers_changed(&gpui::ModifiersChangedEvent::default()), None);
            // Escape with nothing held cancels; with a modifier held it is a key like any other.
            assert_eq!(recorder.on_key_down(&layout.down(1)), Some(Recorded::Cancelled));
            recorder.on_modifiers_changed(&modifier(56, true)); // left Alt
            assert_eq!(recorder.on_key_down(&layout.down(1)), combo(&["Alt", "Escape"]));
            // A mouse button with a modifier held; left and right buttons are never bound.
            let mut recorder = Recorder::new();
            recorder.on_modifiers_changed(&modifier(56, true));
            assert_eq!(recorder.on_mouse_down(gpui::MouseButton::Left), None);
            assert_eq!(
                recorder.on_mouse_down(gpui::MouseButton::Navigate(gpui::NavigationDirection::Back)),
                combo(&["Alt", "Mouse4"])
            );
            // An auto-repeat is not a second press; a key released alone completes it.
            let mut recorder = Recorder::new();
            let mut held = layout.down(39);
            held.is_held = true;
            assert_eq!(recorder.on_key_down(&held), None);
            recorder.on_modifiers_changed(&modifier(125, true)); // left Super
            assert_eq!(recorder.on_key_up(&layout.up(39)), None);
            assert_eq!(recorder.on_modifiers_changed(&modifier(125, false)), combo(&["MetaLeft"]));
        }
    }

    /// The focused fallback fires a binding on the key it was recorded on: Ctrl + the key
    /// right of L, which types ş on a Turkish keyboard and ö on a German one.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    #[gpui::test]
    fn the_focused_fallback_matches_the_physical_key_under_any_layout(cx: &mut TestAppContext) {
        let calls: Rc<RefCell<Calls>> = Rc::default();
        let executor = cx.executor();
        let h = cx.new(|_| Hotkeys::new(backend(calls.clone(), executor)));
        let s = AppSettings {
            hotkeys: vec![binding("m", "toggle_mute", &["Control", "Semicolon"])],
            ..AppSettings::default()
        };
        h.update(cx, |h, cx| {
            h.sync(&s, cx);
            h.set_status(status(true), &s, cx);
        });
        for name in ["tr", "de", "us"] {
            let layout = layouts::Layout::new(name);
            let (down, up) = (layout.down(39), layout.up(39));
            h.update(cx, |h, _| {
                h.key_event(layouts::modifier(29, true).key.unwrap().code, None, true);
                h.key_event(down.code, Some(&down.keystroke), true);
                h.key_event(up.code, Some(&up.keystroke), false);
                h.key_event(layouts::modifier(29, false).key.unwrap().code, None, false);
            });
            assert_eq!(
                calls.borrow().injected.last().unwrap(),
                &(Action::ToggleMute, true, "m".into()),
                "{name}: Ctrl + the {:?} key",
                layout.typed(39)
            );
            calls.borrow_mut().injected.clear();
        }
        // The key that *types* a semicolon on a German keyboard (Shift + Comma) is not it.
        h.update(cx, |h, _| {
            h.key_event(gpui::KeyCode::Evdev(29), None, true);
            h.key_event(gpui::KeyCode::Evdev(51), Some(&gpui::Keystroke::parse(";").unwrap()), true);
        });
        assert!(calls.borrow().injected.is_empty());
    }

    #[test]
    fn combos_and_labels() {
        let v = |k: &[&str]| k.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(normalize_combo(&v(&["ShiftLeft", "ControlRight", "KeyM"])), v(&["Control", "Shift", "KeyM"]));
        assert_eq!(normalize_combo(&v(&["ShiftRight", "ControlLeft"])), v(&["ControlLeft", "ShiftRight"]));
        assert!(same_combo(&v(&["Control", "KeyM"]), &v(&["KeyM", "Control"])));
        assert!(portal_can_bind(&v(&["Control", "KeyM"])));
        assert!(!portal_can_bind(&v(&["Mouse4"])));
        assert!(!portal_can_bind(&v(&["ControlLeft"])));
        assert_eq!(combo_label(&v(&["Control", "KeyM"])), "Ctrl + M");
        assert_eq!(key_label("ShiftRight"), "Right Shift");
        assert_eq!(key_label("Numpad7"), "Num 7");
        assert_eq!(code_from_gpui_key("m").as_deref(), Some("KeyM"));
        assert_eq!(code_from_gpui_key("f9").as_deref(), Some("F9"));
        assert_eq!(code_from_gpui_key("ß"), None);
        let mut s = AppSettings::default();
        store_bindings(&mut s, &[Binding { id: "a".into(), action: Action::LeaveVoice, keys: v(&["F9"]) }]);
        assert_eq!(s.hotkeys[0].action, "leave_voice");
        assert_eq!(bindings(&s)[0].action, Action::LeaveVoice);
    }
}
