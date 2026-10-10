//! XInput2 raw key / button events (X11 sessions, and XWayland on Wayland
//! desktops without the GlobalShortcuts portal). Raw events selected on
//! the root window reach us whatever is focused and whatever grabs exist,
//! and nothing is swallowed — the focused game still gets the key.
//!
//! Through XWayland only X11 clients' input is visible: keys typed into
//! native Wayland windows (Decibell included) are not, which is why the
//! `xwayland` backend asks the renderer for its focused fallback.

use std::sync::atomic::{AtomicBool, Ordering};

use x11rb::connection::Connection;
use x11rb::protocol::xinput::{self, ConnectionExt as _, EventMask, XIEventMask};
use x11rb::protocol::Event;

use super::{keys, raw_key, update_status, Binding};

static RUNNING: AtomicBool = AtomicBool::new(false);

/// The listener stays up once started (an idle X connection costs
/// nothing); bindings live in the shared matcher.
pub fn apply(bindings: &[Binding], xwayland: bool) {
    let backend = if xwayland { "xwayland" } else { "x11" };
    if bindings.is_empty() {
        update_status(|s| {
            s.backend = backend;
            s.state = "idle";
            s.detail = None;
        });
        return;
    }
    if RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    update_status(|s| {
        s.backend = backend;
        s.state = "starting";
        s.detail = None;
    });
    let spawned = std::thread::Builder::new()
        .name("hotkeys-x11".into())
        .spawn(move || {
            let result = listen();
            RUNNING.store(false, Ordering::Release);
            super::release_all_bindings();
            let detail = match result {
                Err(e) => e,
                Ok(()) => "X connection closed".into(),
            };
            log::warn!("[hotkeys] x11: {}", detail);
            update_status(|s| {
                s.state = "error";
                s.detail = Some(detail);
            });
        });
    if let Err(e) = spawned {
        RUNNING.store(false, Ordering::Release);
        update_status(|s| {
            s.state = "error";
            s.detail = Some(format!("thread: {}", e));
        });
    }
}

fn listen() -> Result<(), String> {
    let (conn, screen) = x11rb::connect(None).map_err(|e| format!("connect: {}", e))?;
    let root = conn.setup().roots[screen].root;
    // 2.2: raw events delivered regardless of grabs.
    let version = conn
        .xinput_xi_query_version(2, 2)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| format!("XInput2 unavailable: {}", e))?;
    if (version.major_version, version.minor_version) < (2, 1) {
        return Err(format!(
            "XInput {}.{} is too old (need 2.1)",
            version.major_version, version.minor_version
        ));
    }
    conn.xinput_xi_select_events(
        root,
        &[EventMask {
            // Master devices only: one event per press, not one per
            // slave keyboard as well.
            deviceid: u16::from(xinput::Device::ALL_MASTER),
            mask: vec![
                XIEventMask::RAW_KEY_PRESS
                    | XIEventMask::RAW_KEY_RELEASE
                    | XIEventMask::RAW_BUTTON_PRESS
                    | XIEventMask::RAW_BUTTON_RELEASE,
            ],
        }],
    )
    .map_err(|e| e.to_string())?
    .check()
    .map_err(|e| format!("XISelectEvents: {}", e))?;
    conn.flush().map_err(|e| e.to_string())?;
    update_status(|s| {
        s.state = "active";
        s.detail = None;
    });

    loop {
        match conn.wait_for_event().map_err(|e| e.to_string())? {
            Event::XinputRawKeyPress(e) => key(e.detail, true),
            Event::XinputRawKeyRelease(e) => key(e.detail, false),
            Event::XinputRawButtonPress(e) => button(e.detail, true),
            Event::XinputRawButtonRelease(e) => button(e.detail, false),
            _ => {}
        }
    }
}

fn key(keycode: u32, down: bool) {
    // X keycodes are evdev codes + 8 on every evdev/libinput server.
    if let Some(k) = keycode.checked_sub(8).and_then(|c| u16::try_from(c).ok()).and_then(keys::from_evdev) {
        raw_key(k, down);
    }
}

fn button(button: u32, down: bool) {
    // 1/3 are left/right (unbindable), 4–7 the wheel.
    let k = match button {
        2 => keys::MOUSE_MIDDLE,
        8 => keys::MOUSE_BACK,
        9 => keys::MOUSE_FORWARD,
        _ => return,
    };
    raw_key(k, down);
}
