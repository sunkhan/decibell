//! Windows: system hotkeys for press actions, a poll of the bound keys for holds (owner decision 2026-11-07,
//! amending `docs/superpowers/specs/2026-10-10-global-hotkeys-design.md`).
//!
//! - **Press actions** (mute, deafen, leave, answer, decline) become `RegisterHotKey` system hotkeys on exactly
//!   the bound combo when it has Ctrl / Alt / Win, or when its key types nothing (F1–F24, Pause, Scroll Lock,
//!   media and browser keys). Windows posts one `WM_HOTKEY` per press (`MOD_NOREPEAT`) to a thread that
//!   otherwise sleeps in `GetMessage`: nothing wakes while nothing is pressed. Windows matches the modifiers
//!   exactly, as the matcher does. Like an X11 grab, the combo then no longer reaches the focused app.
//! - **Holds** (push to talk / push to mute), and press bindings `RegisterHotKey` can't take (a mouse button, a
//!   modifier-only combo, a key that types, a combo another app already registered), are polled with
//!   `GetAsyncKeyState`: exactly their keys (plus Ctrl / Shift / Alt / Win when a polled press binding needs its
//!   modifiers to match), every 20 ms (one voice frame), on a thread that exists only while such a binding
//!   does. Their keys still reach the focused app. No hook, no raw-input registration.
//! - Recording a keybind in Settings pauses hotkeys; the registrations are dropped meanwhile so the recorder
//!   sees the keys, and come back after.
//!
//! A polled press shorter than one poll can slip through; real presses last 50 ms and more. Character keys
//! (letters, digits, punctuation) are resolved to virtual keys through the active layout whenever the bindings
//! change, so they stay the physical keys that were recorded.
//!
//! Measured before (all press bindings polled every 10 ms, the eight sided modifiers included): 287 wakeups a
//! second for the polling thread while any hotkey was bound.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, MapVirtualKeyW, RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS, MAPVK_VSC_TO_VK_EX,
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, MOD_WIN,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetMessageW, PeekMessageW, PostThreadMessageW, MSG, PM_NOREMOVE, WM_APP, WM_HOTKEY,
};

use super::{keys, matcher, raw_key, update_status, Action, Binding};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Posted to the hotkey thread: commands are waiting in its channel.
const WM_COMMAND_READY: u32 = WM_APP + 1;

/// A binding `RegisterHotKey` takes.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Registration {
    id: String,
    action: Action,
    /// MOD_* flags without MOD_NOREPEAT.
    mods: u32,
    vk: u32,
}

/// The listener's inputs, kept so a pause / unpause can rebuild the plan.
struct Inputs {
    bindings: Vec<Binding>,
    paused: bool,
}

fn inputs() -> MutexGuard<'static, Inputs> {
    static INPUTS: OnceLock<Mutex<Inputs>> = OnceLock::new();
    INPUTS
        .get_or_init(|| Mutex::new(Inputs { bindings: Vec::new(), paused: false }))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

pub fn apply(bindings: &[Binding]) {
    let mut i = inputs();
    i.bindings = bindings.to_vec();
    reconcile(&i);
}

/// Called by `super::set_paused` (Settings is recording a keybind).
pub fn set_paused(paused: bool) {
    let mut i = inputs();
    if i.paused != paused {
        i.paused = paused;
        reconcile(&i);
    }
}

/// Split the bindings into system hotkeys and polled keys, (re)register, and point the poller at the rest.
/// Holds `inputs()` throughout, so applies and pauses don't interleave.
fn reconcile(i: &Inputs) {
    let (regs, mut polled) = plan(&i.bindings, |k| virtual_key(k).map(|vk| vk as u32));

    // Registrations live on the hotkey thread (RegisterHotKey binds to the calling thread's queue).
    let wanted = if i.paused { Vec::new() } else { regs.clone() };
    let results = hotkey_thread_set(wanted.clone(), !i.bindings.is_empty());
    let mut registered = Vec::new();
    for (r, ok) in wanted.iter().zip(results.iter()) {
        if *ok {
            registered.push(r.id.clone());
        } else if let Some(b) = i.bindings.iter().find(|b| b.id == r.id) {
            // Another app already owns this combo: keep the binding working by polling it (both apps then act).
            polled.push(b.clone());
        }
    }
    let conflicts = wanted.len() - registered.len();

    // The matcher sees only the polled bindings, so a registered press can't also fire from the poll.
    let released = super::matcher().set_bindings(&polled);
    super::fire(released);
    set_poll_bindings(polled);

    let idle = i.bindings.is_empty();
    update_status(|s| {
        s.state = if idle { "idle" } else { "active" };
        s.detail = (conflicts > 0).then(|| {
            format!("{conflicts} combo(s) already registered by another app; Decibell checks those keys instead")
        });
        s.failed.clear();
        s.registered = registered;
    });
}

/// Which bindings become system hotkeys (`Registration`) and which are polled.
fn plan(bindings: &[Binding], vk_of: impl Fn(&str) -> Option<u32>) -> (Vec<Registration>, Vec<Binding>) {
    let mut regs = Vec::new();
    let mut polled = Vec::new();
    for b in bindings {
        match registration(b, &vk_of) {
            Some((mods, vk)) => regs.push(Registration { id: b.id.clone(), action: b.action, mods, vk }),
            None => polled.push(b.clone()),
        }
    }
    (regs, polled)
}

/// `RegisterHotKey`'s (modifiers, virtual key) for a press binding, or None when it must be polled: holds
/// (owner decision: their keys keep reaching apps), mouse buttons, modifier-only and sided-modifier combos,
/// two non-modifier keys, and keys that type unless Ctrl / Alt / Win is part of the combo.
fn registration(b: &Binding, vk_of: &impl Fn(&str) -> Option<u32>) -> Option<(u32, u32)> {
    if b.action.is_hold() {
        return None;
    }
    let mut mods = 0u32;
    let mut main: Option<&str> = None;
    for k in &b.keys {
        if keys::is_mouse(k) || keys::sided_modifier(k).is_some() {
            return None;
        }
        if let Some(m) = keys::generic_modifier(k) {
            mods |= match m {
                keys::Modifier::Control => MOD_CONTROL.0,
                keys::Modifier::Shift => MOD_SHIFT.0,
                keys::Modifier::Alt => MOD_ALT.0,
                keys::Modifier::Meta => MOD_WIN.0,
            };
            continue;
        }
        if main.replace(k.as_str()).is_some() {
            return None;
        }
    }
    let main = main?;
    let command = mods & (MOD_CONTROL.0 | MOD_ALT.0 | MOD_WIN.0) != 0;
    if !command && !types_nothing(main) {
        return None;
    }
    Some((mods, vk_of(main)?))
}

/// Keys that type nothing and that a bare (or Shift-only) binding can take whole: F1–F24, Pause, Scroll Lock,
/// media / volume / browser / launch keys. Everything else bound bare (letters, Space, Enter, arrows…) stays
/// polled so it keeps typing in other apps.
fn types_nothing(key: &str) -> bool {
    key.strip_prefix('F').is_some_and(|n| n.parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)))
        || matches!(key, "Pause" | "ScrollLock")
        || ["Media", "AudioVolume", "Browser", "Launch"].iter().any(|p| key.starts_with(p))
}

fn virtual_key(key: &str) -> Option<i32> {
    if let Some(vk) = keys::win_fixed_vk(key) {
        return Some(vk as i32);
    }
    let scancode = keys::win_scancode(key)?;
    let vk = unsafe { MapVirtualKeyW(scancode as u32, MAPVK_VSC_TO_VK_EX) };
    (vk != 0).then_some(vk as i32)
}

// ── the hotkey thread ─────────────────────────────────────────────────────────────────────────────────

enum Command {
    /// Drop every registration, register these, answer which succeeded.
    Set(Vec<Registration>, mpsc::SyncSender<Vec<bool>>),
    Quit,
}

struct HotkeyThread {
    tx: mpsc::Sender<Command>,
    thread_id: u32,
}

fn hotkey_thread() -> MutexGuard<'static, Option<HotkeyThread>> {
    static THREAD: OnceLock<Mutex<Option<HotkeyThread>>> = OnceLock::new();
    THREAD.get_or_init(|| Mutex::new(None)).lock().unwrap_or_else(|e| e.into_inner())
}

/// Send `regs` to the hotkey thread (starting it when there's something to register) and wait for the
/// results. `keep` = bindings exist: with nothing to register and no bindings, the thread exits.
fn hotkey_thread_set(regs: Vec<Registration>, keep: bool) -> Vec<bool> {
    let mut slot = hotkey_thread();
    if slot.is_none() {
        if regs.is_empty() {
            return Vec::new();
        }
        match spawn_hotkey_thread() {
            Ok(t) => *slot = Some(t),
            Err(e) => {
                update_status(|s| {
                    s.state = "error";
                    s.detail = Some(format!("hotkey thread: {e}"));
                });
                return vec![false; regs.len()];
            }
        }
    }
    let t = slot.as_ref().expect("hotkey thread");
    let quit = regs.is_empty() && !keep;
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    let sent = if quit { t.tx.send(Command::Quit) } else { t.tx.send(Command::Set(regs.clone(), reply_tx)) };
    let posted = sent.is_ok() && unsafe { PostThreadMessageW(t.thread_id, WM_COMMAND_READY, WPARAM(0), LPARAM(0)) }.is_ok();
    if quit || !posted {
        *slot = None;
        return vec![false; regs.len()];
    }
    reply_rx.recv_timeout(Duration::from_secs(2)).unwrap_or_else(|_| vec![false; regs.len()])
}

fn spawn_hotkey_thread() -> Result<HotkeyThread, String> {
    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("hotkeys-system".into())
        .spawn(move || hotkey_loop(rx, ready_tx))
        .map_err(|e| e.to_string())?;
    let thread_id = ready_rx.recv_timeout(Duration::from_secs(2)).map_err(|e| e.to_string())?;
    Ok(HotkeyThread { tx, thread_id })
}

fn hotkey_loop(rx: mpsc::Receiver<Command>, ready: mpsc::SyncSender<u32>) {
    let mut msg = MSG::default();
    // The first PeekMessage gives this thread its message queue, before anyone posts to it.
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_APP, WM_APP, PM_NOREMOVE);
    }
    let _ = ready.send(unsafe { GetCurrentThreadId() });
    // Hotkey id i + 1 → (binding id, action); an empty id is a slot whose registration failed.
    let mut active: Vec<(String, Action)> = Vec::new();
    let unregister_all = |active: &mut Vec<(String, Action)>| {
        for i in 0..active.len() {
            unsafe {
                let _ = UnregisterHotKey(None, i as i32 + 1);
            }
        }
        active.clear();
    };
    loop {
        // Blocks until a hotkey fires or a command is posted: no timer, no wakeups otherwise.
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break; // WM_QUIT or an error
        }
        match msg.message {
            WM_HOTKEY => {
                if let Some((id, action)) = active.get((msg.wParam.0 as usize).wrapping_sub(1)) {
                    if !id.is_empty() {
                        super::dispatch(id, *action, true);
                    }
                }
            }
            WM_COMMAND_READY => {
                while let Ok(cmd) = rx.try_recv() {
                    match cmd {
                        Command::Set(regs, reply) => {
                            unregister_all(&mut active);
                            let mut ok = Vec::with_capacity(regs.len());
                            for r in regs {
                                let id = active.len() as i32 + 1;
                                let mods = HOT_KEY_MODIFIERS(r.mods | MOD_NOREPEAT.0);
                                let good = unsafe { RegisterHotKey(None, id, mods, r.vk) }.is_ok();
                                ok.push(good);
                                active.push((if good { r.id } else { String::new() }, r.action));
                            }
                            let _ = reply.send(ok);
                        }
                        Command::Quit => {
                            unregister_all(&mut active);
                            return;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    unregister_all(&mut active);
}

// ── the poller ────────────────────────────────────────────────────────────────────────────────────────

static POLL_RUNNING: AtomicBool = AtomicBool::new(false);
/// Bumped whenever the polled set changes; the poll thread rebuilds its watch list, and exits when it's empty.
static POLL_GENERATION: AtomicU64 = AtomicU64::new(0);

fn poll_bindings() -> MutexGuard<'static, Vec<Binding>> {
    static POLLED: OnceLock<Mutex<Vec<Binding>>> = OnceLock::new();
    POLLED.get_or_init(|| Mutex::new(Vec::new())).lock().unwrap_or_else(|e| e.into_inner())
}

fn set_poll_bindings(polled: Vec<Binding>) {
    let start = !polled.is_empty();
    *poll_bindings() = polled;
    POLL_GENERATION.fetch_add(1, Ordering::AcqRel);
    if !start || POLL_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(e) = std::thread::Builder::new().name("hotkeys-poll".into()).spawn(poll_loop) {
        POLL_RUNNING.store(false, Ordering::Release);
        update_status(|s| {
            s.state = "error";
            s.detail = Some(format!("thread: {}", e));
        });
    }
}

fn poll_loop() {
    let mut generation = u64::MAX;
    // (key, virtual key, last seen down)
    let mut watched: Vec<(&'static str, i32, bool)> = Vec::new();
    loop {
        let current = POLL_GENERATION.load(Ordering::Acquire);
        if current != generation {
            generation = current;
            let bindings = poll_bindings().clone();
            if bindings.is_empty() {
                POLL_RUNNING.store(false, Ordering::Release);
                // A set racing this exit saw POLL_RUNNING still set and didn't spawn; take it over rather than
                // leave its keys unwatched.
                if poll_bindings().is_empty() || POLL_RUNNING.swap(true, Ordering::AcqRel) {
                    return;
                }
                generation = u64::MAX;
                continue;
            }
            // Start from all-up on both sides of the matcher.
            super::release_all_bindings();
            watched = matcher::watched_keys(&bindings)
                .into_iter()
                .filter_map(|k| Some((k, virtual_key(k)?, false)))
                .collect();
        }
        for (key, vk, down) in watched.iter_mut() {
            // High bit set: the key is down right now.
            let is_down = unsafe { GetAsyncKeyState(*vk) } < 0;
            if is_down != *down {
                *down = is_down;
                raw_key(key, is_down);
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(id: &str, action: Action, keys: &[&str]) -> Binding {
        Binding { id: id.into(), action, keys: keys.iter().map(|k| k.to_string()).collect() }
    }
    /// A layout-free stand-in for MapVirtualKey: every key maps to 0x100 + its name length.
    fn fake_vk(k: &str) -> Option<u32> {
        Some(0x100 + k.len() as u32)
    }
    fn reg(binding: &Binding) -> Option<(u32, u32)> {
        registration(binding, &fake_vk)
    }

    #[test]
    fn press_combos_with_a_command_modifier_register() {
        assert_eq!(reg(&b("a", Action::ToggleMute, &["Control", "KeyM"])), Some((MOD_CONTROL.0, fake_vk("KeyM").unwrap())));
        assert_eq!(
            reg(&b("b", Action::ToggleDeafen, &["Control", "Shift", "KeyD"])).map(|r| r.0),
            Some(MOD_CONTROL.0 | MOD_SHIFT.0)
        );
        assert_eq!(reg(&b("c", Action::LeaveVoice, &["Alt", "Meta", "Digit1"])).map(|r| r.0), Some(MOD_ALT.0 | MOD_WIN.0));
    }

    #[test]
    fn keys_that_type_stay_polled_unless_a_command_modifier_is_held() {
        assert_eq!(reg(&b("a", Action::ToggleMute, &["KeyM"])), None);
        assert_eq!(reg(&b("b", Action::ToggleMute, &["Shift", "KeyM"])), None);
        assert_eq!(reg(&b("c", Action::ToggleMute, &["Space"])), None);
        assert_eq!(reg(&b("d", Action::ToggleMute, &["Enter"])), None);
    }

    #[test]
    fn keys_that_type_nothing_register_bare_or_with_shift() {
        assert_eq!(reg(&b("a", Action::ToggleMute, &["F13"])).map(|r| r.0), Some(0));
        assert_eq!(reg(&b("b", Action::ToggleMute, &["Shift", "F5"])).map(|r| r.0), Some(MOD_SHIFT.0));
        assert!(reg(&b("c", Action::AnswerCall, &["MediaPlayPause"])).is_some());
        assert!(reg(&b("d", Action::DeclineCall, &["Pause"])).is_some());
        assert_eq!(reg(&b("e", Action::ToggleMute, &["F25"])), None);
    }

    #[test]
    fn holds_mouse_buttons_and_modifier_only_combos_stay_polled() {
        assert_eq!(reg(&b("a", Action::PushToTalk, &["KeyV"])), None);
        assert_eq!(reg(&b("b", Action::PushToTalk, &["Control", "F13"])), None);
        assert_eq!(reg(&b("c", Action::PushToMute, &["Mouse4"])), None);
        assert_eq!(reg(&b("d", Action::ToggleMute, &["Mouse5"])), None);
        assert_eq!(reg(&b("e", Action::ToggleMute, &["ControlRight"])), None);
        assert_eq!(reg(&b("f", Action::ToggleMute, &["Control", "Shift"])), None);
        assert_eq!(reg(&b("g", Action::ToggleMute, &["Control", "KeyA", "KeyB"])), None);
    }

    #[test]
    fn plan_splits_bindings() {
        let all = vec![
            b("mute", Action::ToggleMute, &["Control", "Shift", "KeyM"]),
            b("ptt", Action::PushToTalk, &["Mouse4"]),
            b("leave", Action::LeaveVoice, &["KeyL"]),
        ];
        let (regs, polled) = plan(&all, fake_vk);
        assert_eq!(regs.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["mute"]);
        assert_eq!(polled.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(), ["ptt", "leave"]);
    }

    /// The hotkey thread end to end without touching the keyboard: register an obscure combo, post the
    /// WM_HOTKEY Windows would post, and see the action dispatched.
    #[test]
    fn hotkey_thread_dispatches_wm_hotkey() {
        let t = spawn_hotkey_thread().expect("thread");
        let registration = Registration {
            id: "test-mute".into(),
            action: Action::ToggleMute,
            mods: MOD_CONTROL.0 | MOD_ALT.0 | MOD_SHIFT.0 | MOD_WIN.0,
            vk: keys::win_fixed_vk("F24").expect("F24") as u32,
        };
        let (reply_tx, reply_rx) = mpsc::sync_channel(1);
        t.tx.send(Command::Set(vec![registration], reply_tx)).unwrap();
        unsafe { PostThreadMessageW(t.thread_id, WM_COMMAND_READY, WPARAM(0), LPARAM(0)) }.unwrap();
        let ok = reply_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(ok, vec![true], "Ctrl+Alt+Shift+Win+F24 is registered by something else on this machine");

        super::super::tests::FIRED.lock().unwrap().retain(|(a, _)| *a != Action::ToggleMute);
        unsafe { PostThreadMessageW(t.thread_id, WM_HOTKEY, WPARAM(1), LPARAM(0)) }.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let fired = loop {
            if super::super::tests::FIRED.lock().unwrap().contains(&(Action::ToggleMute, true)) {
                break true;
            }
            if std::time::Instant::now() > deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        t.tx.send(Command::Quit).unwrap();
        unsafe { PostThreadMessageW(t.thread_id, WM_COMMAND_READY, WPARAM(0), LPARAM(0)) }.unwrap();
        assert!(fired, "WM_HOTKEY didn't dispatch the binding's action");
    }
}
