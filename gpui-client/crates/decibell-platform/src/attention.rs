//! "Taskbar flash" (`docs/FEATURES.md` §9; `window.ts`' `decibell:window:flash`): an incoming call
//! asks for the user's attention without taking the focus, unless the window is focused. The model
//! decides when (`Cue::Flash` from `ring_incoming`: every incoming call, never under Do Not
//! Disturb); this answers it.
//!
//! - Wayland: an xdg-activation request with a token tied to no input (GPUI patch 0015): the
//!   compositor marks the window as demanding attention; KWin flashes its taskbar entry until the
//!   window is activated.
//! - X11: the ICCCM urgency hint (GPUI's own `request_attention`), cleared by GPUI on focus.
//! - Windows: `FlashWindowEx(FLASHW_TRAY | FLASHW_TIMERNOFG)` until the window comes to the
//!   foreground, as Electron's `flashFrame(true)`.
//!
//! No timer: the desktop ends the flash when the window is activated.

use std::sync::atomic::{AtomicU64, Ordering};

use decibell_model::Model;
use decibell_model::live::Cue;
use gpui::{AnyWindowHandle, App, Subscription, Window};

/// How many requests were made (a request is made only while the window is not active); for the
/// `attention` self-test, beside what the compositor reports.
pub static REQUESTS: AtomicU64 = AtomicU64::new(0);

pub(crate) fn install(window: AnyWindowHandle, cx: &mut App) -> Option<Subscription> {
    let model = cx.try_global::<Model>()?.clone();
    Some(cx.subscribe(&model.live.cues, move |_, cue: &Cue, cx| {
        if *cue == Cue::Flash {
            let _ = window.update(cx, |_, window, _| request(window));
        }
    }))
}

/// Asks for attention unless the window is active (`if (!w || w.isFocused()) return`).
pub fn request(window: &mut Window) {
    if window.is_window_active() {
        return;
    }
    REQUESTS.fetch_add(1, Ordering::Relaxed);
    crate::marker(format_args!("attention requested"));
    #[cfg(windows)]
    crate::win32::flash(window);
    #[cfg(not(windows))]
    window.request_attention();
}
