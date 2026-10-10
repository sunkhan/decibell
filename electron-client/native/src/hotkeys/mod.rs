//! Global hotkeys: keybinds that work while Decibell isn't focused.
//! Design: docs/superpowers/specs/2026-10-10-global-hotkeys-design.md.
//!
//! One listener per platform feeds `dispatch`. Every one of them learns
//! about the bound keys only — never a stream of everything typed:
//! - Linux Wayland: the GlobalShortcuts portal (`portal.rs`); the desktop
//!   matches keys and reports only our shortcuts. Falls back to the X11
//!   listener through XWayland when the desktop has no such portal;
//! - Linux X11: passive grabs on exactly the bound combos, then the bound
//!   keys' state until release (`x11.rs`);
//! - Windows: the state of the bound keys only, polled (`windows.rs`);
//! - macOS: none here — Electron main registers accelerators and injects.
//!
//! `dispatch` drives push-to-talk / push-to-mute straight into the voice
//! gate and tells the renderer about every transition (`hotkey_action`),
//! which runs the press actions (mute, deafen, leave, answer, decline).

pub mod keys;
pub mod matcher;
#[cfg(target_os = "linux")]
mod portal;
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "linux")]
mod x11;

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};

use crate::media::voice_gate;
use matcher::{Fired, Matcher};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    ToggleMute,
    ToggleDeafen,
    PushToTalk,
    PushToMute,
    LeaveVoice,
    AnswerCall,
    DeclineCall,
}

impl Action {
    pub fn parse(s: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
    }

    /// Held actions have a release; the rest fire on press only.
    pub fn is_hold(self) -> bool {
        matches!(self, Action::PushToTalk | Action::PushToMute)
    }

    /// Shown in the desktop's shortcut settings (portal description).
    pub fn label(self) -> &'static str {
        match self {
            Action::ToggleMute => "Toggle mute",
            Action::ToggleDeafen => "Toggle deafen",
            Action::PushToTalk => "Push to talk",
            Action::PushToMute => "Push to mute",
            Action::LeaveVoice => "Leave voice / hang up",
            Action::AnswerCall => "Answer call",
            Action::DeclineCall => "Decline call",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub id: String,
    pub action: Action,
    /// DOM `KeyboardEvent.code`s, generic modifiers, `Mouse3..5`.
    pub keys: Vec<String>,
}

/// What the Keybinds tab shows about the listener on this machine.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// "portal" | "x11" | "xwayland" | "windows" | "electron" (macOS) | "none"
    pub backend: &'static str,
    /// "idle" (no bindings) | "starting" | "active" | "error"
    pub state: &'static str,
    pub detail: Option<String>,
    /// The listener sees mouse buttons.
    pub mouse: bool,
    /// The listener misses keys while Decibell itself is focused; the
    /// renderer should match window key events and inject them.
    pub focused_fallback: bool,
    /// Portal v2: `hotkeys_open_system_settings` opens the desktop's editor.
    pub can_configure: bool,
    /// Binding id → the trigger the desktop actually assigned (portal).
    pub triggers: HashMap<String, String>,
    /// Binding ids the listener couldn't register (X11: another app
    /// already grabs that combo).
    pub failed: Vec<String>,
}

fn initial_backend() -> &'static str {
    #[cfg(target_os = "linux")]
    return linux_backend().name();
    #[cfg(target_os = "windows")]
    return "windows";
    // Electron main registers press actions as accelerators.
    #[cfg(target_os = "macos")]
    return "electron";
    #[allow(unreachable_code)]
    "none"
}

impl Status {
    fn initial() -> Self {
        let backend = initial_backend();
        Status {
            backend,
            state: "idle",
            detail: None,
            mouse: matches!(backend, "x11" | "windows"),
            focused_fallback: matches!(backend, "electron" | "none"),
            can_configure: false,
            triggers: HashMap::new(),
            failed: Vec::new(),
        }
    }
}

struct Hub {
    bindings: Vec<Binding>,
    paused: bool,
    /// Held sources per hold action (binding ids, or "cli").
    held: HashMap<Action, HashSet<String>>,
    status: Status,
}

fn hub() -> MutexGuard<'static, Hub> {
    static HUB: OnceLock<Mutex<Hub>> = OnceLock::new();
    HUB.get_or_init(|| {
        Mutex::new(Hub {
            bindings: Vec::new(),
            paused: false,
            held: HashMap::new(),
            status: Status::initial(),
        })
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

fn matcher() -> MutexGuard<'static, Matcher> {
    static MATCHER: OnceLock<Mutex<Matcher>> = OnceLock::new();
    MATCHER
        .get_or_init(|| Mutex::new(Matcher::default()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The CLI (`decibell --hotkey=…`) source. Survives rebinding and pause
/// so a compositor's key-up still lands.
pub const CLI_SOURCE: &str = "cli";

/// One press / release from any source. `source` identifies what is held
/// (binding id, or `CLI_SOURCE`) so two bindings for the same hold action
/// don't release each other.
pub fn dispatch(source: &str, action: Action, pressed: bool) {
    let transition = {
        let mut h = hub();
        if pressed && h.paused {
            return;
        }
        if action.is_hold() {
            let set = h.held.entry(action).or_default();
            let before = !set.is_empty();
            if pressed {
                set.insert(source.to_string());
            } else {
                set.remove(source);
            }
            let after = !set.is_empty();
            if before == after {
                return;
            }
            match action {
                Action::PushToTalk => voice_gate::set_ptt_held(after),
                Action::PushToMute => voice_gate::set_ptm_held(after),
                _ => {}
            }
            (action, after)
        } else if pressed {
            (action, true)
        } else {
            return;
        }
    };
    emit_action(transition.0, transition.1);
}

// The event bus is a napi threadsafe function, which can't link into the
// `cargo test` binary; tests only observe the hub's state.
#[cfg(not(test))]
fn emit_action(action: Action, pressed: bool) {
    crate::events::emit_hotkey_action(action, pressed);
}
#[cfg(test)]
fn emit_action(action: Action, pressed: bool) {
    tests::FIRED.lock().unwrap().push((action, pressed));
}

fn fire(fired: Vec<Fired>) {
    for f in fired {
        dispatch(&f.id, f.action, f.pressed);
    }
}

/// A raw key / button transition from a matcher-based listener.
#[allow(dead_code)] // unused on macOS
pub(crate) fn raw_key(key: &'static str, down: bool) {
    #[cfg(test)]
    tests::RAW_KEYS.lock().unwrap().push(key);
    let fired = matcher().key(key, down);
    fire(fired);
}

/// Release everything held through bindings (not the CLI).
pub(crate) fn release_all_bindings() {
    let released = matcher().release_all();
    fire(released);
    let held: Vec<(Action, String)> = hub()
        .held
        .iter()
        .flat_map(|(a, set)| set.iter().map(move |s| (*a, s.clone())))
        .filter(|(_, s)| s != CLI_SOURCE)
        .collect();
    for (action, source) in held {
        dispatch(&source, action, false);
    }
}

pub fn configure(bindings: Vec<Binding>) {
    set_bindings(&bindings);
    apply_backend(&bindings);
}

/// Hub + matcher half of `configure`; the backend is applied after.
fn set_bindings(bindings: &[Binding]) {
    release_all_bindings();
    let released = matcher().set_bindings(bindings);
    fire(released);
    hub().bindings = bindings.to_vec();
}

pub fn set_paused(paused: bool) {
    hub().paused = paused;
    if paused {
        release_all_bindings();
    }
}

pub fn status() -> Status {
    hub().status.clone()
}

pub(crate) fn update_status(f: impl FnOnce(&mut Status)) {
    let snapshot = {
        let mut h = hub();
        f(&mut h.status);
        h.status.clone()
    };
    #[cfg(not(test))]
    crate::events::emit_hotkeys_status(&snapshot);
    #[cfg(test)]
    let _ = snapshot;
}

/// The current bindings, for listeners that (re)build their watch list.
#[allow(dead_code)] // unused on macOS
pub(crate) fn bindings() -> Vec<Binding> {
    hub().bindings.clone()
}

/// Open the desktop's own shortcut editor (portal v2).
pub fn open_system_settings() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        portal::open_settings()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err("No system shortcut editor on this platform".into())
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinuxBackend {
    Portal,
    X11,
    None,
}

#[cfg(target_os = "linux")]
impl LinuxBackend {
    fn name(self) -> &'static str {
        match self {
            LinuxBackend::Portal => "portal",
            LinuxBackend::X11 => "x11",
            LinuxBackend::None => "none",
        }
    }
}

#[cfg(target_os = "linux")]
fn linux_backend() -> LinuxBackend {
    let set = |v: &str| std::env::var_os(v).is_some_and(|s| !s.is_empty());
    if set("WAYLAND_DISPLAY") {
        LinuxBackend::Portal
    } else if set("DISPLAY") {
        LinuxBackend::X11
    } else {
        LinuxBackend::None
    }
}

fn apply_backend(bindings: &[Binding]) {
    #[cfg(target_os = "linux")]
    match linux_backend() {
        LinuxBackend::Portal => portal::apply(bindings),
        LinuxBackend::X11 => x11::apply(bindings, false),
        LinuxBackend::None => {}
    }
    #[cfg(target_os = "windows")]
    windows::apply(bindings);
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let _ = bindings;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `hotkey_action` the hub would have emitted (tests only).
    pub(super) static FIRED: Mutex<Vec<(Action, bool)>> = Mutex::new(Vec::new());
    /// Every key a listener reported to the matcher (tests only).
    pub(super) static RAW_KEYS: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

    /// The X11 listener end to end against a disposable X server with
    /// XTEST (e.g. `Xvfb :97`), driven by synthetic input:
    /// `DECIBELL_X11_TEST_DISPLAY=:97 cargo test --lib x11_listener -- --ignored`.
    /// Never point it at a real session: it grabs keys there.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs a disposable X server with XTEST (Xvfb)"]
    fn x11_listener_end_to_end() {
        use std::time::Duration;
        use x11rb::connection::Connection;
        use x11rb::protocol::xtest::ConnectionExt as _;

        let display = std::env::var("DECIBELL_X11_TEST_DISPLAY").expect("DECIBELL_X11_TEST_DISPLAY");
        std::env::set_var("DISPLAY", &display);
        let (conn, screen) = x11rb::connect(Some(&display)).expect("test X server");
        let root = conn.setup().roots[screen].root;
        // xproto event codes for XTEST: 2/3 key press/release, 4/5 button.
        let fake = |kind: u8, detail: u8| {
            conn.xtest_fake_input(kind, detail, 0, root, 0, 0, 0).unwrap();
            conn.flush().unwrap();
            std::thread::sleep(Duration::from_millis(60));
        };
        let fake_now = |kind: u8, detail: u8| {
            conn.xtest_fake_input(kind, detail, 0, root, 0, 0, 0).unwrap();
        };
        let settle = || std::thread::sleep(Duration::from_millis(150));
        let take = || std::mem::take(&mut *FIRED.lock().unwrap());
        const CONTROL_L: u8 = 37;
        const SHIFT_L: u8 = 50;
        const KEY_M: u8 = 58;
        const KEY_A: u8 = 38;
        const KEY_S: u8 = 39;
        const SPACE: u8 = 65;

        let bindings = vec![
            Binding { id: "mute".into(), action: Action::ToggleMute, keys: vec!["Control".into(), "KeyM".into()] },
            Binding { id: "ptt".into(), action: Action::PushToTalk, keys: vec!["Mouse4".into()] },
        ];
        set_bindings(&bindings);
        x11::apply(&bindings, false);
        settle();
        assert_eq!(status().state, "active", "{:?}", status().detail);
        assert!(status().failed.is_empty());
        take();
        RAW_KEYS.lock().unwrap().clear();

        // Ctrl+M fires toggle mute once, auto-repeat-free.
        fake(2, CONTROL_L);
        fake(2, KEY_M);
        fake(3, KEY_M);
        fake(3, CONTROL_L);
        settle();
        assert_eq!(take(), vec![(Action::ToggleMute, true)]);

        // Exact modifiers: Ctrl+Shift+M is not Ctrl+M; M alone isn't either.
        fake(2, CONTROL_L);
        fake(2, SHIFT_L);
        fake(2, KEY_M);
        fake(3, KEY_M);
        fake(3, SHIFT_L);
        fake(3, CONTROL_L);
        fake(2, KEY_M);
        fake(3, KEY_M);
        settle();
        assert_eq!(take(), vec![]);

        // Push to talk on a side button: press and release both arrive,
        // and the voice gate sees the hold.
        fake(4, 8);
        settle();
        assert_eq!(take(), vec![(Action::PushToTalk, true)]);
        assert!(hub().held.get(&Action::PushToTalk).is_some_and(|s| !s.is_empty()));
        fake(5, 8);
        settle();
        assert_eq!(take(), vec![(Action::PushToTalk, false)]);

        // A tap faster than the 10 ms sampling still counts once.
        fake(2, CONTROL_L);
        fake_now(2, KEY_M);
        fake_now(3, KEY_M);
        conn.flush().unwrap();
        settle();
        fake(3, CONTROL_L);
        settle();
        assert_eq!(take(), vec![(Action::ToggleMute, true)]);

        // Typing unbound keys — idle, and while push-to-talk is held (the
        // listener is sampling then) — fires nothing, and no key outside
        // the watch list ever reaches the matcher.
        for kc in [KEY_A, KEY_S, SPACE] {
            fake(2, kc);
            fake(3, kc);
        }
        fake(4, 8);
        for kc in [KEY_A, KEY_S, SPACE] {
            fake(2, kc);
            fake(3, kc);
        }
        fake(5, 8);
        settle();
        assert_eq!(take(), vec![(Action::PushToTalk, true), (Action::PushToTalk, false)]);
        let watched = matcher::watched_keys(&bindings);
        let seen = std::mem::take(&mut *RAW_KEYS.lock().unwrap());
        assert!(!seen.is_empty());
        for k in &seen {
            assert!(watched.contains(k), "listener reported unbound key {}", k);
        }

        // Unbinding drops every grab and ends the listener thread.
        set_bindings(&[]);
        x11::apply(&[], false);
        settle();
        fake(2, CONTROL_L);
        fake(2, KEY_M);
        fake(3, KEY_M);
        fake(3, CONTROL_L);
        settle();
        assert_eq!(take(), vec![]);
        assert_eq!(status().state, "idle");
    }

    #[test]
    fn action_names_round_trip() {
        for a in [
            Action::ToggleMute,
            Action::ToggleDeafen,
            Action::PushToTalk,
            Action::PushToMute,
            Action::LeaveVoice,
            Action::AnswerCall,
            Action::DeclineCall,
        ] {
            let name = serde_json::to_value(a).unwrap();
            assert_eq!(Action::parse(name.as_str().unwrap()), Some(a));
        }
        assert_eq!(Action::parse("toggle_mute"), Some(Action::ToggleMute));
        assert_eq!(Action::parse("launch_rockets"), None);
    }
}
