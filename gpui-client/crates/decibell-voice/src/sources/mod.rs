//! What can be shared on Windows: the screens and the windows, each with a still.
//!
//! The renderer gets this list from Chromium's `desktopCapturer` (`electron/main/index.ts`,
//! `decibell:capture:listSources`); the core has no such list (it only opens the source it is
//! given, `media/source_id.rs`). So the client makes it, in [`windows`], with the ids the
//! core parses: `screen:<N>:0`, N the `EnumDisplayDevicesW` index of an active display (how
//! WebRTC numbers screens, and what `capture_wgc::resolve_monitor` resolves), and
//! `window:<HWND>:0`, the handle in decimal.
//!
//! Listing blocks (GDI copies the stills) and runs on a background thread, only while the Go
//! Live dialog is open: [`crate::go_live`] owns the task and drops it with the dialog.
//!
//! Elsewhere there is nothing to list (the desktop's portal picks on Linux), except for
//! `DECIBELL_GO_LIVE_SOURCES=fixture` in a development build: a made-up list, so that the grid
//! can be looked at and tested without Windows.

#[cfg(windows)]
mod windows;

/// The two tabs of the grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Screen,
    Window,
}

/// Pixels as GPUI draws them: straight-alpha BGRA, top row first, `width * height * 4` bytes.
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub bgra: Vec<u8>,
}

/// One screen or window.
pub struct ListedSource {
    /// What `StartScreenShareArgs::source_id` takes.
    pub id: String,
    pub name: String,
    pub kind: SourceKind,
    /// A still of it; `None` when it was not asked for or could not be taken.
    pub thumbnail: Option<Bitmap>,
    /// A window's application icon; `None` when not asked for or the window has none.
    pub icon: Option<Bitmap>,
}

/// The largest still of each kind (`SourceGrid.tsx`: 640 × 360 for screens, 320 × 180 for
/// windows), in device pixels.
pub fn thumbnail_box(kind: SourceKind) -> (u32, u32) {
    match kind {
        SourceKind::Screen => (640, 360),
        SourceKind::Window => (320, 180),
    }
}

/// `size` scaled down to fit `bounds`, keeping its shape; never up, never empty.
pub fn fit_within(size: (u32, u32), bounds: (u32, u32)) -> (u32, u32) {
    let (width, height) = (size.0.max(1) as u64, size.1.max(1) as u64);
    let (max_width, max_height) = (bounds.0.max(1) as u64, bounds.1.max(1) as u64);
    if width <= max_width && height <= max_height {
        return (width as u32, height as u32);
    }
    // The tighter of the two ratios, in integers.
    if width * max_height >= height * max_width {
        (max_width as u32, ((height * max_width) / width).max(1) as u32)
    } else {
        (((width * max_height) / height).max(1) as u32, max_height as u32)
    }
}

/// Whether this build shows the in-app grid at all.
pub fn supported() -> bool {
    cfg!(windows) || fixture_requested()
}

fn fixture_requested() -> bool {
    cfg!(any(debug_assertions, feature = "fixtures"))
        && std::env::var("DECIBELL_GO_LIVE_SOURCES").is_ok_and(|value| value == "fixture")
}

/// Every screen, then every window that can be shared. Stills are taken for `thumbnails`
/// only (the tab on screen: the other keeps the stills it had), icons for the windows
/// `wants_icon` says yes to (those it has none for yet). Blocks; call it off the main thread.
pub fn list(
    thumbnails: SourceKind,
    wants_icon: &dyn Fn(&str) -> bool,
) -> Result<Vec<ListedSource>, String> {
    #[cfg(windows)]
    return windows::list(thumbnails, wants_icon);
    #[cfg(not(windows))]
    {
        if fixture_requested() {
            Ok(fixture(thumbnails, wants_icon))
        } else {
            Err("the system's own dialog picks what to share on this platform".into())
        }
    }
}

/// Two screens and five windows, drawn as flat colours.
#[cfg(not(windows))]
fn fixture(thumbnails: SourceKind, wants_icon: &dyn Fn(&str) -> bool) -> Vec<ListedSource> {
    let flat = |width: u32, height: u32, bgr: [u8; 3]| Bitmap {
        width,
        height,
        bgra: (0..width * height)
            .flat_map(|index| {
                // A darker band along the top, so a still is not one colour.
                let band = if index / width < height / 6 { 2 } else { 1 };
                [bgr[0] / band, bgr[1] / band, bgr[2] / band, 255]
            })
            .collect(),
    };
    let screens = [("screen:0:0", "Screen 1", [96, 72, 48]), ("screen:1:0", "Screen 2", [48, 72, 96])];
    let windows = [
        ("window:1001:0", "Decibell design review — Figma", [120, 60, 160], (1600, 900)),
        ("window:1002:0", "main.rs — decibell", [60, 60, 60], (1280, 1400)),
        ("window:1003:0", "Inbox", [40, 120, 200], (900, 600)),
        ("window:1004:0", "A window whose title is long enough that it has to be cut off somewhere", [40, 160, 90], (2560, 600)),
        ("window:1005:0", "Calculator", [30, 30, 140], (320, 480)),
    ];
    let mut list = Vec::new();
    for (id, name, colour) in screens {
        let size = fit_within((2560, 1440), thumbnail_box(SourceKind::Screen));
        list.push(ListedSource {
            id: id.into(),
            name: name.into(),
            kind: SourceKind::Screen,
            thumbnail: (thumbnails == SourceKind::Screen).then(|| flat(size.0, size.1, colour)),
            icon: None,
        });
    }
    for (id, name, colour, size) in windows {
        let fitted = fit_within(size, thumbnail_box(SourceKind::Window));
        list.push(ListedSource {
            id: id.into(),
            name: name.into(),
            kind: SourceKind::Window,
            thumbnail: (thumbnails == SourceKind::Window).then(|| flat(fitted.0, fitted.1, colour)),
            // One window without an icon, as some have none.
            icon: (wants_icon(id) && id != "window:1005:0").then(|| flat(32, 32, colour)),
        });
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stills_fit_their_box_and_keep_their_shape() {
        assert_eq!(fit_within((2560, 1440), (640, 360)), (640, 360));
        assert_eq!(fit_within((3440, 1440), (640, 360)), (640, 267));
        assert_eq!(fit_within((1080, 1920), (640, 360)), (202, 360));
        // Never enlarged, never empty.
        assert_eq!(fit_within((200, 100), (320, 180)), (200, 100));
        assert_eq!(fit_within((0, 0), (320, 180)), (1, 1));
        assert_eq!(fit_within((100_000, 1), (320, 180)), (320, 1));
    }
}
