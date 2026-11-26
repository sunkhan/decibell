//! The Windows calls of the platform services, against `windows` 0.62 (the bindings GPUI's own
//! Windows platform uses). Written to compile and checked against the bindings' source; nothing here
//! has run (`HANDOFF.md`, "Windows batch"; the owner's checks in `docs/windows-owner-checks.md`).

use std::cell::RefCell;
use std::io::{Read, Write};

use futures::channel::mpsc::UnboundedSender;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_NO_DATA, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE,
    HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_FIRST_PIPE_INSTANCE, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, FlushFileBuffers,
    OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeServerProcessId, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT, WaitNamedPipeW,
};
use windows::Win32::System::RemoteDesktop::{NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification};
use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO};
use windows::Win32::UI::Shell::{ITaskbarList3, TBPF_NOPROGRESS, TBPF_NORMAL, TBPF_PAUSED, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, CreateWindowExW, DefWindowProcW, DispatchMessageW, FLASHW_TIMERNOFG, FLASHW_TRAY,
    FLASHWINFO, FlashWindowEx, GetMessageW, HWND_MESSAGE, MSG, RegisterClassW, TranslateMessage, WINDOW_EX_STYLE,
    WINDOW_STYLE, WM_WTSSESSION_CHANGE, WNDCLASSW, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};
use windows::core::{HSTRING, PCWSTR, w};

/// The window's HWND (GPUI's window implements `HasWindowHandle` with a `Win32` handle).
pub fn hwnd(window: &gpui::Window) -> Option<HWND> {
    // The trait's method by name: `Window::window_handle` is GPUI's own (an `AnyWindowHandle`).
    match HasWindowHandle::window_handle(window).ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut core::ffi::c_void)),
        _ => None,
    }
}

/// A second start lets the running instance take the foreground (Windows lets only a process that
/// may set the foreground window itself hand that right on: the one the user just started).
pub fn allow_foreground(pid: u32) {
    // SAFETY: a plain call with a process id; no pointers.
    if let Err(e) = unsafe { AllowSetForegroundWindow(pid) } {
        log::debug!("[instance] AllowSetForegroundWindow({pid}): {e}");
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
    // SAFETY: `info` is a fully initialised FLASHWINFO that outlives the call; `hwnd` is this
    // process's live window. The return value is the previous state, not an error.
    let _ = unsafe { FlashWindowEx(&info) };
}

/// The taskbar button's progress (`setProgressBar`): `ITaskbarList3` on the UI thread, which GPUI
/// initialised as a single-threaded COM apartment (`OleInitialize`).
#[derive(Default)]
pub struct TaskbarProgress {
    list: Option<ITaskbarList3>,
    failed: bool,
}

impl TaskbarProgress {
    /// `None` clears it; `Some((fraction, paused))` shows it.
    pub fn set(&mut self, window: &gpui::Window, value: Option<(f64, bool)>) {
        let Some(hwnd) = hwnd(window) else { return };
        if self.list.is_none() && !self.failed {
            // SAFETY: COM is initialised on this thread (GPUI's UI thread); the CLSID is the
            // shell's TaskbarList and no outer object is passed.
            let created: windows::core::Result<ITaskbarList3> = unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) };
            match created {
                // SAFETY: a fresh interface pointer, initialised once before use as documented.
                Ok(list) => match unsafe { list.HrInit() } {
                    Ok(()) => self.list = Some(list),
                    Err(e) => {
                        log::warn!("[progress] ITaskbarList3::HrInit: {e}");
                        self.failed = true;
                    }
                },
                Err(e) => {
                    log::warn!("[progress] no ITaskbarList3: {e}");
                    self.failed = true;
                }
            }
        }
        let Some(list) = &self.list else { return };
        let result = match value {
            // SAFETY: a live, initialised interface and this process's own top-level window.
            None => unsafe { list.SetProgressState(hwnd, TBPF_NOPROGRESS) },
            Some((fraction, paused)) => {
                // SAFETY: as above.
                let state = unsafe { list.SetProgressState(hwnd, if paused { TBPF_PAUSED } else { TBPF_NORMAL }) };
                let done = (fraction.clamp(0.0, 1.0) * 1000.0).round() as u64;
                // SAFETY: as above.
                state.and_then(|()| unsafe { list.SetProgressValue(hwnd, done, 1000) })
            }
        };
        if let Err(e) = result {
            log::warn!("[progress] taskbar: {e}");
        }
    }
}

/// How long the session has had no keyboard or mouse input (`GetLastInputInfo`, in the same
/// 32-bit millisecond clock as `GetTickCount`; the subtraction wraps as that clock does every
/// 49.7 days). `None` if Windows doesn't answer.
pub fn idle_for() -> Option<std::time::Duration> {
    let mut info = LASTINPUTINFO { cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    // SAFETY: `info` is a valid LASTINPUTINFO with `cbSize` set, alive for the call.
    if !unsafe { GetLastInputInfo(&mut info) }.as_bool() {
        return None;
    }
    // SAFETY: no arguments.
    let now = unsafe { GetTickCount() };
    Some(std::time::Duration::from_millis(u64::from(now.wrapping_sub(info.dwTime))))
}

// ── The instance pipe ──────────────────────────────────────────────────────────────────────────

/// A pipe handle. HANDLE is a raw pointer, so not `Send`; a pipe handle may be used from any
/// thread, and each of these is used by one thread at a time (the listener, or the second start).
struct OwnedPipe(HANDLE);

// SAFETY: kernel handles are process-wide; this one is only used by the thread that owns it.
unsafe impl Send for OwnedPipe {}

impl Drop for OwnedPipe {
    fn drop(&mut self) {
        // SAFETY: the handle is open and owned by this value; it is closed once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The first instance's end: one pipe instance (`FILE_FLAG_FIRST_PIPE_INSTANCE`: creating it fails
/// if another process already serves the name), reused for every second start.
pub struct Pipe {
    handle: OwnedPipe,
}

impl Pipe {
    pub fn create(name: &str) -> windows::core::Result<Pipe> {
        let name = HSTRING::from(name);
        // SAFETY: `name` is a NUL-terminated wide string alive for the call; no security
        // attributes (the default DACL: the creator and administrators may write).
        let handle = unsafe {
            CreateNamedPipeW(
                &name,
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                64 * 1024,
                0,
                None,
            )
        };
        if handle.is_invalid() {
            return Err(windows::core::Error::from_thread());
        }
        Ok(Pipe { handle: OwnedPipe(handle) })
    }

    /// Blocks until a second start connects (the listener thread, so nothing wakes at rest).
    pub fn accept(&self) -> windows::core::Result<()> {
        loop {
            // SAFETY: a live pipe handle in blocking mode; no OVERLAPPED.
            match unsafe { ConnectNamedPipe(self.handle.0, None) } {
                Ok(()) => return Ok(()),
                // A client connected between creating (or disconnecting) and this call.
                Err(e) if e.code() == ERROR_PIPE_CONNECTED.to_hresult() => return Ok(()),
                // A client connected and already left: free the instance, wait for the next.
                Err(e) if e.code() == ERROR_NO_DATA.to_hresult() => self.disconnect(),
                Err(e) => return Err(e),
            }
        }
    }

    pub fn stream(&self) -> PipeStream<'_> {
        PipeStream(&self.handle.0)
    }

    /// Ends this connection (after the answer reached the client) so the next can come.
    pub fn disconnect(&self) {
        // SAFETY: a live pipe handle; flushing waits until the client has read what was written.
        unsafe {
            let _ = FlushFileBuffers(self.handle.0);
            let _ = DisconnectNamedPipe(self.handle.0);
        }
    }
}

/// The second start's end.
pub struct PipeClient {
    handle: OwnedPipe,
}

impl PipeClient {
    /// Opens the running instance's pipe; `Err` when nobody serves it yet (the caller retries).
    pub fn open(name: &str) -> anyhow::Result<PipeClient> {
        let name = HSTRING::from(name);
        for _ in 0..3 {
            // SAFETY: `name` is a NUL-terminated wide string alive for the call; no security
            // attributes or template.
            let opened = unsafe {
                CreateFileW(
                    &name,
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            };
            match opened {
                Ok(handle) => return Ok(PipeClient { handle: OwnedPipe(handle) }),
                // Another second start is being served: wait for the pipe (up to 2 s), then retry.
                Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() => {
                    // SAFETY: as above; the return value only says whether it waited successfully.
                    let _ = unsafe { WaitNamedPipeW(&name, 2000) };
                }
                Err(e) => return Err(anyhow::anyhow!("opening {name}: {e}")),
            }
        }
        anyhow::bail!("the instance pipe stayed busy")
    }

    /// The running instance's process id (to let it take the foreground).
    pub fn server_process_id(&self) -> Option<u32> {
        let mut pid = 0u32;
        // SAFETY: a live client handle of a named pipe; `pid` is a valid u32 to write.
        unsafe { GetNamedPipeServerProcessId(self.handle.0, &mut pid) }.ok()?;
        (pid != 0).then_some(pid)
    }

    pub fn stream(&self) -> PipeStream<'_> {
        PipeStream(&self.handle.0)
    }
}

/// Blocking reads and writes on a pipe handle (`ReadFile` / `WriteFile`, no OVERLAPPED).
pub struct PipeStream<'a>(&'a HANDLE);

fn io_error(e: windows::core::Error) -> std::io::Error {
    if e.code() == ERROR_BROKEN_PIPE.to_hresult() {
        return std::io::Error::new(std::io::ErrorKind::UnexpectedEof, e);
    }
    std::io::Error::other(e)
}

impl Read for PipeStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut read = 0u32;
        // ReadFile takes at most u32::MAX bytes.
        let len = buf.len().min(u32::MAX as usize);
        // SAFETY: a live pipe handle opened for reading; `buf[..len]` and `read` are valid for
        // writes for the call's duration.
        match unsafe { ReadFile(*self.0, Some(&mut buf[..len]), Some(&mut read), None) } {
            Ok(()) => Ok(read as usize),
            Err(e) if e.code() == ERROR_BROKEN_PIPE.to_hresult() => Ok(0),
            Err(e) => Err(io_error(e)),
        }
    }
}

impl Write for PipeStream<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut written = 0u32;
        let len = buf.len().min(u32::MAX as usize);
        // SAFETY: a live pipe handle opened for writing; `buf[..len]` and `written` are valid for
        // the call's duration.
        unsafe { WriteFile(*self.0, Some(&buf[..len]), Some(&mut written), None) }.map_err(io_error)?;
        Ok(written as usize)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ── Screen lock ────────────────────────────────────────────────────────────────────────────────

thread_local! {
    /// Where the session window's procedure sends lock (`true`) / unlock (`false`); set by the
    /// watcher thread before its window exists, read only on that thread.
    static SESSION_EVENTS: RefCell<Option<UnboundedSender<bool>>> = const { RefCell::new(None) };
}

unsafe extern "system" fn session_window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_WTSSESSION_CHANGE {
        let locked = match wparam.0 as u32 {
            WTS_SESSION_LOCK => Some(true),
            WTS_SESSION_UNLOCK => Some(false),
            _ => None,
        };
        if let Some(locked) = locked {
            SESSION_EVENTS.with(|events| {
                if let Some(tx) = events.borrow().as_ref() {
                    tx.unbounded_send(locked).ok();
                }
            });
        }
        return LRESULT(0);
    }
    // SAFETY: the arguments are exactly those Windows passed to this window procedure.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// Screen lock / unlock as `WM_WTSSESSION_CHANGE`, received by a message-only window on a thread of
/// its own (`decibell-session`) that sleeps in `GetMessageW` until the session changes.
pub fn watch_session_lock(tx: UnboundedSender<bool>) {
    let spawned = std::thread::Builder::new().name("decibell-session".into()).spawn(move || {
        SESSION_EVENTS.with(|events| *events.borrow_mut() = Some(tx));
        if let Err(e) = run_session_window() {
            log::warn!("[session] no screen-lock notifications: {e}");
        }
    });
    if let Err(e) = spawned {
        log::warn!("[session] couldn't start the session watcher: {e}");
    }
}

fn run_session_window() -> windows::core::Result<()> {
    // SAFETY: `None` asks for this executable's module; the handle is not freed by us.
    let module = unsafe { GetModuleHandleW(None)? };
    let instance: HINSTANCE = module.into();
    let class: PCWSTR = w!("DecibellSessionWatcher");
    let class_info = WNDCLASSW { lpfnWndProc: Some(session_window_proc), hInstance: instance, lpszClassName: class, ..Default::default() };
    // SAFETY: `class_info` is fully initialised and outlives the call; the class name is static.
    if unsafe { RegisterClassW(&class_info) } == 0 {
        return Err(windows::core::Error::from_thread());
    }
    // SAFETY: a message-only window (parent HWND_MESSAGE) of the class registered just above, on
    // this thread, with no creation parameter.
    let window = unsafe {
        CreateWindowExW(WINDOW_EX_STYLE(0), class, PCWSTR::null(), WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance), None)?
    };
    // SAFETY: `window` is the live window just created on this thread.
    unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION)? };
    let mut message = MSG::default();
    // SAFETY: `message` is a valid MSG for every call; the loop ends on WM_QUIT (0) or an error
    // (-1). The window lives as long as the process.
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        // SAFETY: `message` was just filled by GetMessageW.
        unsafe {
            let _ = TranslateMessage(&message);
            let _ = DispatchMessageW(&message);
        }
    }
    Ok(())
}
