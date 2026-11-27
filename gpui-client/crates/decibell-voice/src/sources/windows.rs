//! The list on Windows: active displays and shareable top-level windows, with GDI stills.
//!
//! Which windows: what Chromium's picker lists (WebRTC's `GetWindowList` with its usual
//! flags): visible, not minimised (the core refuses a minimised window), titled, not a tool
//! window, not owned unless marked as an application window, not cloaked (another virtual
//! desktop, a suspended store app), answering messages, not the shell's desktop, and not ours.
//!
//! Stills: a screen is copied from the desktop (`StretchBlt` from the screen DC); a window is
//! asked to draw itself (`PrintWindow` with `PW_RENDERFULLCONTENT`, which also gets windows
//! that are covered or drawn by DirectComposition) and falls back to what is on screen at its
//! place. Everything fails soft: a source that vanished between being listed and being copied
//! simply has no still.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_CLOAKED, DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CAPTUREBLT, CreateCompatibleBitmap,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DISPLAY_DEVICE_ACTIVE, DISPLAY_DEVICEW,
    DeleteDC, DeleteObject, EnumDisplayDevicesW, EnumDisplayMonitors, GetDC, GetMonitorInfoW,
    HALFTONE, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFO, MONITORINFOEXW, ReleaseDC, SRCCOPY,
    SelectObject, SetBrushOrgEx, SetStretchBltMode, StretchBlt,
};
use windows::Win32::Storage::Xps::{PRINT_WINDOW_FLAGS, PrintWindow};
use windows::Win32::System::Threading::GetCurrentProcessId;
use windows::Win32::UI::WindowsAndMessaging::{
    DI_NORMAL, DrawIconEx, EnumWindows, GCLP_HICON, GCLP_HICONSM, GW_OWNER, GWL_EXSTYLE,
    GetClassLongPtrW, GetClassNameW, GetWindow, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, HICON, ICON_BIG, ICON_SMALL, ICON_SMALL2, IsIconic, IsWindowVisible,
    SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_GETICON, WM_NULL, WS_EX_APPWINDOW,
    WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PCWSTR};

use super::{Bitmap, ListedSource, SourceKind, fit_within, thumbnail_box};

/// `PrintWindow`'s flag for "what the compositor shows, child and DirectComposition content
/// included" (Windows 8.1 and later; `PW_RENDERFULLCONTENT` in WinUser.h).
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);
/// How long a window gets to answer before it counts as hung, in milliseconds (WebRTC's 50).
const ANSWER_MS: u32 = 50;
/// The side of an icon bitmap: 16 logical pixels at up to 200 % scale.
const ICON_SIDE: i32 = 32;

pub(super) fn list(
    thumbnails: SourceKind,
    wants_icon: &dyn Fn(&str) -> bool,
) -> Result<Vec<ListedSource>, String> {
    let mut sources = Vec::new();
    let screens = screens();
    let single = screens.len() == 1;
    for (order, screen) in screens.iter().enumerate() {
        sources.push(ListedSource {
            id: format!("screen:{}:0", screen.device_index),
            // Chromium's names.
            name: if single {
                "Entire screen".to_string()
            } else {
                format!("Screen {}", order + 1)
            },
            kind: SourceKind::Screen,
            thumbnail: (thumbnails == SourceKind::Screen)
                .then(|| screen_still(screen.bounds))
                .flatten(),
            icon: None,
        });
    }
    for window in shareable_windows() {
        let id = format!("window:{}:0", window.hwnd.0 as usize);
        sources.push(ListedSource {
            thumbnail: (thumbnails == SourceKind::Window)
                .then(|| window_still(window.hwnd))
                .flatten(),
            icon: wants_icon(&id).then(|| window_icon(window.hwnd)).flatten(),
            id,
            name: window.title,
            kind: SourceKind::Window,
        });
    }
    if sources.is_empty() {
        return Err("Windows listed no screen and no window".into());
    }
    Ok(sources)
}

struct Screen {
    /// The index `EnumDisplayDevicesW` gave the display: WebRTC's screen id, the N of
    /// `screen:N:0`.
    device_index: u32,
    /// Where it is on the desktop, in pixels.
    bounds: RECT,
}

fn same_name(left: &[u16], right: &[u16]) -> bool {
    let end = |name: &[u16]| name.iter().position(|&unit| unit == 0).unwrap_or(name.len());
    left[..end(left)] == right[..end(right)]
}

/// The active displays in device order, each with the monitor rectangle of the same device
/// name. A display without a monitor (should not happen) is left out.
fn screens() -> Vec<Screen> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _dc: HDC,
        _rect: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        // Safety: `data` is the `Vec` passed to `EnumDisplayMonitors` below, alive for the call.
        let monitors = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        monitors.push(monitor);
        BOOL(1)
    }
    let mut handles: Vec<HMONITOR> = Vec::new();
    // Safety: the callback only pushes to `handles`, which outlives the call.
    let _ = unsafe {
        EnumDisplayMonitors(None, None, Some(collect), LPARAM(&raw mut handles as isize))
    };
    let monitors: Vec<([u16; 32], RECT)> = handles
        .into_iter()
        .filter_map(|monitor| {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
            // Safety: `info` is a MONITORINFOEXW with its size set, which the call may fill.
            unsafe { GetMonitorInfoW(monitor, (&raw mut info).cast::<MONITORINFO>()) }
                .as_bool()
                .then_some((info.szDevice, info.monitorInfo.rcMonitor))
        })
        .collect();

    let mut screens = Vec::new();
    for device_index in 0..64 {
        let mut device = DISPLAY_DEVICEW {
            cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // Safety: `device` has its size set. False: no device at this index, the end.
        if !unsafe { EnumDisplayDevicesW(PCWSTR::null(), device_index, &mut device, 0) }.as_bool() {
            break;
        }
        if !device.StateFlags.contains(DISPLAY_DEVICE_ACTIVE) {
            continue;
        }
        if let Some((_, bounds)) = monitors
            .iter()
            .find(|(name, _)| same_name(name, &device.DeviceName))
        {
            screens.push(Screen {
                device_index,
                bounds: *bounds,
            });
        }
    }
    screens
}

struct ShareableWindow {
    hwnd: HWND,
    title: String,
}

fn shareable_windows() -> Vec<ShareableWindow> {
    unsafe extern "system" fn collect(hwnd: HWND, data: LPARAM) -> BOOL {
        // Safety: `data` is the `Vec` passed to `EnumWindows` below, alive for the call.
        let windows = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
        windows.push(hwnd);
        BOOL(1)
    }
    let mut handles: Vec<HWND> = Vec::new();
    // Safety: the callback only pushes to `handles`, which outlives the call. Top to bottom
    // in z-order, which is the order the renderer's list has.
    if unsafe { EnumWindows(Some(collect), LPARAM(&raw mut handles as isize)) }.is_err() {
        return Vec::new();
    }
    // Safety: no arguments.
    let own_process = unsafe { GetCurrentProcessId() };
    handles
        .into_iter()
        .filter_map(|hwnd| {
            // Safety: queries on a window handle; each fails harmlessly if the window is gone.
            unsafe {
                if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
                    return None;
                }
                let mut process = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut process));
                if process == own_process {
                    return None;
                }
                let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                if style & WS_EX_TOOLWINDOW.0 != 0 {
                    return None;
                }
                let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.0.is_null());
                if owned && style & WS_EX_APPWINDOW.0 == 0 {
                    return None;
                }
                let mut cloaked = 0u32;
                if DwmGetWindowAttribute(
                    hwnd,
                    DWMWA_CLOAKED,
                    (&raw mut cloaked).cast::<c_void>(),
                    std::mem::size_of::<u32>() as u32,
                )
                .is_ok()
                    && cloaked != 0
                {
                    return None;
                }
                let mut class = [0u16; 64];
                let length = GetClassNameW(hwnd, &mut class).max(0) as usize;
                let class = String::from_utf16_lossy(&class[..length.min(class.len())]);
                // The desktop itself and the Start button of old shells.
                if class == "Progman" || class == "Button" {
                    return None;
                }
                let mut rect = RECT::default();
                if GetWindowRect(hwnd, &mut rect).is_err()
                    || rect.right <= rect.left
                    || rect.bottom <= rect.top
                {
                    return None;
                }
                // A hung window would hang whoever asks it for its title or its picture.
                if !answers(hwnd) {
                    return None;
                }
                let mut title = [0u16; 512];
                let length = GetWindowTextW(hwnd, &mut title).max(0) as usize;
                let title = String::from_utf16_lossy(&title[..length.min(title.len())]);
                let title = title.trim();
                if title.is_empty() {
                    return None;
                }
                Some(ShareableWindow {
                    hwnd,
                    title: title.to_string(),
                })
            }
        })
        .collect()
}

/// Whether the window's thread answers a message within [`ANSWER_MS`].
fn answers(hwnd: HWND) -> bool {
    let mut result = 0usize;
    // Safety: a no-op message with a timeout; `result` is a valid out pointer.
    unsafe {
        SendMessageTimeoutW(
            hwnd,
            WM_NULL,
            WPARAM(0),
            LPARAM(0),
            SMTO_ABORTIFHUNG,
            ANSWER_MS,
            Some(&mut result),
        )
    }
    .0 != 0
}

/// A memory DC with a top-down 32-bit bitmap selected into it, and the bitmap's pixels.
struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Canvas {
    fn new(width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 || width > 16_384 || height > 16_384 {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                // Negative: the first row in memory is the top one.
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        // Safety: plain GDI object creation; every handle is checked and released in `Drop`
        // (or right here on the failure paths).
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return None;
            }
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            {
                Ok(bitmap) if !bits.is_null() => bitmap,
                Ok(bitmap) => {
                    let _ = DeleteObject(bitmap.into());
                    let _ = DeleteDC(dc);
                    return None;
                }
                Err(_) => {
                    let _ = DeleteDC(dc);
                    return None;
                }
            };
            let previous = SelectObject(dc, bitmap.into());
            Some(Self {
                dc,
                bitmap,
                previous,
                bits: bits.cast(),
                width,
                height,
            })
        }
    }

    /// The pixels as GDI left them: BGRA, the alpha byte whatever the drawing call wrote.
    fn pixels(&self) -> Vec<u8> {
        let length = self.width as usize * self.height as usize * 4;
        // Safety: the DIB section is `width * height` 32-bit pixels, alive until `Drop`, and
        // GDI has finished writing it (GDI calls are synchronous for a DIB section).
        unsafe { std::slice::from_raw_parts(self.bits, length) }.to_vec()
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // Safety: our own objects; the bitmap is deselected before it is deleted.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// The desktop's DC, released when dropped.
struct ScreenDc(HDC);

impl ScreenDc {
    fn get() -> Option<Self> {
        // Safety: the DC of the whole desktop; released in `Drop`.
        let dc = unsafe { GetDC(None) };
        (!dc.is_invalid()).then_some(Self(dc))
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // Safety: paired with `GetDC(None)`.
        unsafe { ReleaseDC(None, self.0) };
    }
}

/// Scales `source`'s rectangle from `source_dc` into a new still that fits `kind`'s box.
fn scaled_still(source_dc: HDC, source: RECT, kind: SourceKind, capture_layered: bool) -> Option<Bitmap> {
    let (width, height) = (source.right - source.left, source.bottom - source.top);
    if width <= 0 || height <= 0 {
        return None;
    }
    let (still_width, still_height) = fit_within((width as u32, height as u32), thumbnail_box(kind));
    let canvas = Canvas::new(still_width as i32, still_height as i32)?;
    let operation = if capture_layered { SRCCOPY | CAPTUREBLT } else { SRCCOPY };
    // Safety: both DCs are valid for the call; HALFTONE averages the source pixels (and asks
    // for the brush origin to be set).
    let copied = unsafe {
        SetStretchBltMode(canvas.dc, HALFTONE);
        let _ = SetBrushOrgEx(canvas.dc, 0, 0, None);
        StretchBlt(
            canvas.dc,
            0,
            0,
            canvas.width,
            canvas.height,
            Some(source_dc),
            source.left,
            source.top,
            width,
            height,
            operation,
        )
    };
    if !copied.as_bool() {
        return None;
    }
    let mut bgra = canvas.pixels();
    // GDI leaves the alpha byte of copied pixels at 0: the still is opaque.
    for pixel in bgra.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
    Some(Bitmap {
        width: still_width,
        height: still_height,
        bgra,
    })
}

fn screen_still(bounds: RECT) -> Option<Bitmap> {
    let screen = ScreenDc::get()?;
    // CAPTUREBLT: layered windows are on the desktop too.
    scaled_still(screen.0, bounds, SourceKind::Screen, true)
}

fn window_still(hwnd: HWND) -> Option<Bitmap> {
    let mut window = RECT::default();
    // Safety: a valid out pointer; fails if the window is gone.
    unsafe { GetWindowRect(hwnd, &mut window) }.ok()?;
    // The frame as drawn: the window rectangle of a resizable window includes invisible
    // borders. Falls back to the window rectangle.
    let mut frame = window;
    // Safety: the attribute is a RECT; `frame` is one.
    let framed = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&raw mut frame).cast::<c_void>(),
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if framed.is_err() || frame.right <= frame.left || frame.bottom <= frame.top {
        frame = window;
    }
    let screen = ScreenDc::get()?;
    let (width, height) = (window.right - window.left, window.bottom - window.top);
    if width <= 0 || height <= 0 || width > 16_384 || height > 16_384 {
        return None;
    }
    // The window draws itself into a bitmap of its own size; the still is scaled from the
    // part of it the frame covers.
    // Safety: GDI objects created, used and released here; `PrintWindow` fails (returns
    // false) for a window that is gone or refuses.
    let printed = unsafe {
        let dc = CreateCompatibleDC(Some(screen.0));
        if dc.is_invalid() {
            return None;
        }
        let bitmap = CreateCompatibleBitmap(screen.0, width, height);
        if bitmap.is_invalid() {
            let _ = DeleteDC(dc);
            return None;
        }
        let previous = SelectObject(dc, bitmap.into());
        let still = if PrintWindow(hwnd, dc, PW_RENDERFULLCONTENT).as_bool() {
            let inside = RECT {
                left: frame.left - window.left,
                top: frame.top - window.top,
                right: frame.right - window.left,
                bottom: frame.bottom - window.top,
            };
            scaled_still(dc, inside, SourceKind::Window, false)
        } else {
            None
        };
        SelectObject(dc, previous);
        let _ = DeleteObject(bitmap.into());
        let _ = DeleteDC(dc);
        still
    };
    // A window that will not print: what the desktop shows where it is (right unless it is
    // covered).
    printed.or_else(|| scaled_still(screen.0, frame, SourceKind::Window, true))
}

/// The window's small icon, as the taskbar would pick it.
fn window_icon(hwnd: HWND) -> Option<Bitmap> {
    let asked = |which: u32| -> Option<HICON> {
        let mut result = 0usize;
        // Safety: WM_GETICON returns an icon handle the window owns; with a timeout, so a
        // hung window costs `ANSWER_MS`.
        let answered = unsafe {
            SendMessageTimeoutW(
                hwnd,
                WM_GETICON,
                WPARAM(which as usize),
                LPARAM(0),
                SMTO_ABORTIFHUNG,
                ANSWER_MS,
                Some(&mut result),
            )
        };
        (answered.0 != 0 && result != 0).then_some(HICON(result as *mut c_void))
    };
    // Safety: class data queries on a window handle; 0 when there is none.
    let class_icon = |index| -> Option<HICON> {
        let handle = unsafe { GetClassLongPtrW(hwnd, index) };
        (handle != 0).then_some(HICON(handle as *mut c_void))
    };
    let icon = asked(ICON_SMALL2)
        .or_else(|| asked(ICON_SMALL))
        .or_else(|| class_icon(GCLP_HICONSM))
        .or_else(|| asked(ICON_BIG))
        .or_else(|| class_icon(GCLP_HICON))?;
    let canvas = Canvas::new(ICON_SIDE, ICON_SIDE)?;
    // Safety: the canvas starts zeroed (a new DIB section) and the icon handle belongs to the
    // window or its class; a stale handle makes the call fail.
    unsafe { DrawIconEx(canvas.dc, 0, 0, icon, ICON_SIDE, ICON_SIDE, 0, None, DI_NORMAL) }.ok()?;
    let mut bgra = canvas.pixels();
    let pixels = bgra.as_chunks_mut::<4>().0;
    if pixels.iter().all(|pixel| pixel[3] == 0) {
        // An icon without an alpha channel (a colour bitmap and a mask): what was drawn is
        // opaque, the rest stays clear.
        for pixel in pixels.iter_mut() {
            if pixel[0] != 0 || pixel[1] != 0 || pixel[2] != 0 {
                pixel[3] = 255;
            }
        }
    } else {
        // Drawn over clear black, the colours come out multiplied by their alpha; GPUI takes
        // straight alpha.
        for pixel in pixels.iter_mut() {
            let alpha = pixel[3] as u32;
            if alpha != 0 && alpha != 255 {
                for channel in &mut pixel[..3] {
                    *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
                }
            }
        }
    }
    if pixels.iter().all(|pixel| pixel[3] == 0) {
        return None;
    }
    Some(Bitmap {
        width: ICON_SIDE as u32,
        height: ICON_SIDE as u32,
        bgra,
    })
}
