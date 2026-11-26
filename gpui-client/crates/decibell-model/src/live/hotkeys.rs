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
//! combos, and `Mouse3`..`Mouse5`; the core speaks the same spelling. GPUI's `Keystroke`
//! at the pin carries the layout's character, not the physical key, so
//! [`code_from_gpui_key`] maps it assuming a US layout: the recorder needs the platform's
//! scancode from GPUI to be exact (docs/model.md M3, gaps).

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

/// GPUI's `Keystroke::key` → a `KeyboardEvent.code`, **assuming a US layout** (GPUI
/// gives the layout's character, not the physical key; see the module doc).
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
        }
    }

    pub fn configuring(&self) -> bool {
        self.configuring.is_some()
    }

    pub fn fallback_armed(&self) -> bool {
        self.fallback.is_some()
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
        let fired = self.matcher.key(key, down);
        self.inject(fired);
    }

    /// Our window lost focus: keys released meanwhile never reach us.
    pub fn window_blurred(&mut self) {
        if self.fallback.is_some() {
            let fired = self.matcher.release_all();
            self.inject(fired);
        }
    }
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
