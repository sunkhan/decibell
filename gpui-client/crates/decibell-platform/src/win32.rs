//! The Windows calls (written to compile; nothing here has run: `HANDOFF.md` "Windows batch").

use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{ITaskbarList3, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, FLASHW_TIMERNOFG, FLASHW_TRAY, FLASHWINFO, FlashWindowEx,
};

/// The window's HWND.
pub fn hwnd(window: &gpui::Window) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

/// A second start lets the running instance take the foreground.
pub fn allow_foreground(pid: u32) {
    // SAFETY: plain call with a process id.
    if let Err(e) = unsafe { AllowSetForegroundWindow(pid) } {
        log::debug!("[instance] AllowSetForegroundWindow: {e}");
    }
}

/// `BrowserWindow.flashFrame(true)`: the taskbar button flashes until the window comes to the
/// foreground (`FLASHW_TRAY | FLASHW_TIMERNOFG`). GPUI's own `request_attention` flashes once.
pub fn flash(window: &gpui::Window) {
    let Some(hwnd) = hwnd(window) else { return };
    let info = FLASHWINFO {
        cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
        hwnd,
        dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
        uCount: 0,
        dwTimeout: 0,
    };
    // SAFETY: `info` is a valid FLASHWINFO for the call's duration.
    let _ = unsafe { FlashWindowEx(&info) };
}

/// The taskbar button's progress (`setProgressBar`): `ITaskbarList3` on the UI thread (an STA:
/// GPUI calls `OleInitialize` there).
#[derive(Default)]
pub struct TaskbarProgress {
    list: Option<ITaskbarList3>,
}

impl TaskbarProgress {
    /// `None` clears it; `Some((fraction, paused))` shows it.
    pub fn set(&mut self, window: &gpui::Window, value: Option<(f64, bool)>) {
        let Some(hwnd) = hwnd(window) else { return };
        if self.list.is_none() {
            // SAFETY: COM is initialised on this (the UI) thread.
            let created: windows::core::Result<ITaskbarList3> = unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) };
            match created {
                // SAFETY: a fresh interface pointer.
                Ok(list) => match unsafe { list.HrInit() } {
                    Ok(()) => self.list = Some(list),
                    Err(e) => log::warn!("[progress] ITaskbarList3::HrInit: {e}"),
                },
                Err(e) => log::warn!("[progress] no ITaskbarList3: {e}"),
            }
        }
        let Some(list) = &self.list else { return };
        let result = match value {
            // SAFETY (all three): a live interface and the window's own HWND.
            None => unsafe { list.SetProgressState(hwnd, TBPF_NOPROGRESS) },
            Some((fraction, paused)) => {
                let state = unsafe { list.SetProgressState(hwnd, if paused { TBPF_PAUSED } else { TBPF_NORMAL }) };
                state.and_then(|()| unsafe { list.SetProgressValue(hwnd, (fraction.clamp(0.0, 1.0) * 1000.0) as u64, 1000) })
            }
        };
        if let Err(e) = result {
            log::warn!("[progress] taskbar: {e}");
        }
    }
}
