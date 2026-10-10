//! Windows Raw Input: a message-only window registered for keyboard (and,
//! while a binding uses one, mouse) input with RIDEV_INPUTSINK, so
//! WM_INPUT arrives whatever is focused. Passive — nothing is swallowed,
//! the focused game still gets the key. No hook, so no LowLevelHooksTimeout
//! to trip. Like every non-elevated listener (Discord's included) it sees
//! nothing while an elevated window has focus (UIPI).
//!
//! Scancodes are normalised to Chromium's spelling (`MakeCode | 0xe000`
//! for E0-extended keys) so they match the DOM codes bindings use.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{MapVirtualKeyW, MAPVK_VK_TO_VSC_EX, VK_NUMLOCK, VK_PAUSE};
use windows::Win32::UI::Input::{
    GetRawInputData, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RAWKEYBOARD,
    RAWMOUSE, RIDEV_INPUTSINK, RIDEV_REMOVE, RID_INPUT, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, RegisterClassExW, HWND_MESSAGE, MSG,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_INPUT, WNDCLASSEXW,
};

use super::{keys, raw_key, update_status, Binding};

/// Posted by `apply` so the window thread (re)registers the mouse.
const WM_REFRESH: u32 = WM_APP + 1;

const RI_KEY_BREAK: u16 = 1;
const RI_KEY_E0: u16 = 2;
const RI_KEY_E1: u16 = 4;
/// The fake Shift Windows wraps around E0 sequences (NumLock games).
const VK_FAKE: u16 = 0xff;

const USAGE_PAGE_GENERIC: u16 = 0x01;
const USAGE_MOUSE: u16 = 0x02;
const USAGE_KEYBOARD: u16 = 0x06;

static RUNNING: AtomicBool = AtomicBool::new(false);
static WINDOW: AtomicIsize = AtomicIsize::new(0);
/// Only touched on the window thread.
static MOUSE_REGISTERED: AtomicBool = AtomicBool::new(false);

pub fn apply(bindings: &[Binding]) {
    if !RUNNING.swap(true, Ordering::AcqRel) {
        if bindings.is_empty() {
            RUNNING.store(false, Ordering::Release);
            return;
        }
        update_status(|s| {
            s.state = "starting";
            s.detail = None;
        });
        let spawned = std::thread::Builder::new().name("hotkeys-rawinput".into()).spawn(|| {
            let result = unsafe { listen() };
            RUNNING.store(false, Ordering::Release);
            WINDOW.store(0, Ordering::Release);
            super::release_all_bindings();
            let detail = result.err().unwrap_or_else(|| "message loop ended".into());
            log::warn!("[hotkeys] raw input: {}", detail);
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
        return;
    }
    // Already listening: the matcher has the new bindings; the window
    // thread only needs to add or drop the mouse.
    let hwnd = WINDOW.load(Ordering::Acquire);
    if hwnd != 0 {
        let _ = unsafe { PostMessageW(Some(HWND(hwnd as _)), WM_REFRESH, WPARAM(0), LPARAM(0)) };
    }
    update_status(|s| {
        s.state = if bindings.is_empty() { "idle" } else { "active" };
        s.detail = None;
    });
}

unsafe fn listen() -> Result<(), String> {
    let instance: HINSTANCE = GetModuleHandleW(PCWSTR::null()).map_err(|e| format!("GetModuleHandle: {}", e))?.into();
    let class = w!("DecibellHotkeys");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        lpszClassName: class,
        ..Default::default()
    };
    // 0 also when a previous listener thread registered it already.
    RegisterClassExW(&wc);
    let hwnd = CreateWindowExW(
        WINDOW_EX_STYLE::default(),
        class,
        w!(""),
        WINDOW_STYLE::default(),
        0,
        0,
        0,
        0,
        Some(HWND_MESSAGE),
        None,
        Some(instance),
        None,
    )
    .map_err(|e| format!("CreateWindowEx: {}", e))?;
    WINDOW.store(hwnd.0 as isize, Ordering::Release);

    let keyboard = RAWINPUTDEVICE {
        usUsagePage: USAGE_PAGE_GENERIC,
        usUsage: USAGE_KEYBOARD,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };
    RegisterRawInputDevices(&[keyboard], std::mem::size_of::<RAWINPUTDEVICE>() as u32)
        .map_err(|e| format!("RegisterRawInputDevices: {}", e))?;
    refresh_mouse(hwnd);
    update_status(|s| {
        s.state = "active";
        s.detail = None;
    });

    let mut msg = MSG::default();
    loop {
        let r = GetMessageW(&mut msg, None, 0, 0).0;
        if r == 0 || r == -1 {
            return Err(format!("GetMessage returned {}", r));
        }
        DispatchMessageW(&msg);
    }
}

/// Every mouse move is a WM_INPUT, so the mouse is registered only while
/// a binding uses a mouse button. Registration is per process and per
/// usage: removing only what we added leaves anyone else's alone.
unsafe fn refresh_mouse(hwnd: HWND) {
    let want = super::uses_mouse();
    if want == MOUSE_REGISTERED.load(Ordering::Relaxed) {
        return;
    }
    let device = RAWINPUTDEVICE {
        usUsagePage: USAGE_PAGE_GENERIC,
        usUsage: USAGE_MOUSE,
        dwFlags: if want { RIDEV_INPUTSINK } else { RIDEV_REMOVE },
        hwndTarget: if want { hwnd } else { HWND::default() },
    };
    match RegisterRawInputDevices(&[device], std::mem::size_of::<RAWINPUTDEVICE>() as u32) {
        Ok(()) => MOUSE_REGISTERED.store(want, Ordering::Relaxed),
        Err(e) => log::warn!("[hotkeys] raw mouse {}: {}", if want { "register" } else { "remove" }, e),
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_INPUT => {
            on_input(HRAWINPUT(lparam.0 as _));
            // Lets the system clean up the input buffer.
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
        WM_REFRESH => {
            refresh_mouse(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

unsafe fn on_input(handle: HRAWINPUT) {
    let mut raw = RAWINPUT::default();
    let mut size = std::mem::size_of::<RAWINPUT>() as u32;
    let n = GetRawInputData(
        handle,
        RID_INPUT,
        Some(&mut raw as *mut RAWINPUT as *mut _),
        &mut size,
        std::mem::size_of::<RAWINPUTHEADER>() as u32,
    );
    if n == 0 || n == u32::MAX {
        return;
    }
    if raw.header.dwType == RIM_TYPEKEYBOARD.0 {
        on_keyboard(&raw.data.keyboard);
    } else if raw.header.dwType == RIM_TYPEMOUSE.0 {
        on_mouse(&raw.data.mouse);
    }
}

unsafe fn on_keyboard(kb: &RAWKEYBOARD) {
    if kb.VKey == VK_FAKE {
        return;
    }
    let down = kb.Flags & RI_KEY_BREAK == 0;
    let scancode: u16 = if kb.VKey == VK_PAUSE.0 {
        // Pause arrives as the E1 1D 45 sequence; Chromium calls it 0x45.
        0x0045
    } else if kb.Flags & RI_KEY_E1 != 0 {
        return;
    } else if kb.VKey == VK_NUMLOCK.0 {
        // NumLock reports MakeCode 0x45 without E0; Chromium's is 0xe045.
        0xe045
    } else if kb.MakeCode == 0 {
        // Some HID keyboards send no scancode; derive it (E0 in the high byte).
        MapVirtualKeyW(kb.VKey as u32, MAPVK_VK_TO_VSC_EX) as u16
    } else {
        kb.MakeCode | if kb.Flags & RI_KEY_E0 != 0 { 0xe000 } else { 0 }
    };
    if let Some(k) = keys::from_win_scancode(scancode) {
        raw_key(k, down);
    }
}

unsafe fn on_mouse(mouse: &RAWMOUSE) {
    const BUTTONS: [(u16, u16, &str); 3] = [
        (0x0010, 0x0020, keys::MOUSE_MIDDLE),  // RI_MOUSE_MIDDLE_BUTTON_DOWN / _UP
        (0x0040, 0x0080, keys::MOUSE_BACK),    // RI_MOUSE_BUTTON_4_DOWN / _UP
        (0x0100, 0x0200, keys::MOUSE_FORWARD), // RI_MOUSE_BUTTON_5_DOWN / _UP
    ];
    let flags = mouse.Anonymous.Anonymous.usButtonFlags;
    for (down_bit, up_bit, key) in BUTTONS {
        if flags & down_bit != 0 {
            raw_key(key, true);
        }
        if flags & up_bit != 0 {
            raw_key(key, false);
        }
    }
}
