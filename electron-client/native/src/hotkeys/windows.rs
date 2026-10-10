//! Windows: polls the state of the bound keys only.
//!
//! `GetAsyncKeyState` answers for one key at a time, so Decibell asks about
//! exactly the keys its bindings name — plus Ctrl / Shift / Alt / Win when a
//! press binding needs its modifiers to match — every 10 ms, and is never
//! told about any other key. No hook, no raw-input registration, nothing
//! swallowed: the focused game still gets the key. The thread exists only
//! while at least one binding does.
//!
//! A press shorter than one poll can slip through; real presses last 50 ms
//! and more. Character keys (letters, digits, punctuation) are resolved to
//! virtual keys through the active layout whenever the bindings change, so
//! they stay the physical keys that were recorded.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, MapVirtualKeyW, MAPVK_VSC_TO_VK_EX};

use super::{keys, matcher, raw_key, update_status, Binding};

const POLL_INTERVAL: Duration = Duration::from_millis(10);

static RUNNING: AtomicBool = AtomicBool::new(false);
/// Bumped by every `apply`; the poll thread rebuilds its watch list when
/// it changes, and exits when the bindings are gone.
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn apply(bindings: &[Binding]) {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    update_status(|s| {
        s.state = if bindings.is_empty() { "idle" } else { "active" };
        s.detail = None;
        s.failed.clear();
    });
    if bindings.is_empty() || RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    if let Err(e) = std::thread::Builder::new().name("hotkeys-poll".into()).spawn(poll_loop) {
        RUNNING.store(false, Ordering::Release);
        update_status(|s| {
            s.state = "error";
            s.detail = Some(format!("thread: {}", e));
        });
    }
}

fn virtual_key(key: &str) -> Option<i32> {
    if let Some(vk) = keys::win_fixed_vk(key) {
        return Some(vk as i32);
    }
    let scancode = keys::win_scancode(key)?;
    let vk = unsafe { MapVirtualKeyW(scancode as u32, MAPVK_VSC_TO_VK_EX) };
    (vk != 0).then_some(vk as i32)
}

fn poll_loop() {
    let mut generation = u64::MAX;
    // (key, virtual key, last seen down)
    let mut watched: Vec<(&'static str, i32, bool)> = Vec::new();
    loop {
        let current = GENERATION.load(Ordering::Acquire);
        if current != generation {
            generation = current;
            let bindings = super::bindings();
            if bindings.is_empty() {
                RUNNING.store(false, Ordering::Release);
                // A bind racing this exit saw RUNNING still set and didn't
                // spawn; take it over rather than leave it unwatched.
                if super::bindings().is_empty() || RUNNING.swap(true, Ordering::AcqRel) {
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
