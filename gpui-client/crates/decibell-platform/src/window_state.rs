//! The main window's size, place and maximised state, per profile (`window-state.json`), restored
//! at start: Electron's window opened at 1280 × 800 every time (`window.ts`); here the last run's
//! window comes back, at least 800 × 600.
//!
//! - **Position** only where the platform lets an application place its window: X11 and Windows.
//!   Wayland's compositor places windows itself: no position is saved or asked for there.
//! - **Which bounds**: what reopens the same window (`current`): Wayland's `window_bounds`, X11's
//!   `inner_window_bounds`. A maximised window keeps the windowed size it came from. X11 (KWin):
//!   a window *opened* maximised comes back from it within the client-side margin (20 px) of the
//!   saved size, depending on when KWin learns the margin (`_GTK_FRAME_EXTENTS`) relative to the
//!   maximise; measured 980 and 1020 for a saved 1000.
//! - **Off every screen** (a monitor that is gone): the saved place is dropped and the window opens
//!   centred, as on a first start; a size larger than every screen is shrunk to the largest.
//! - **Saving without a timer**: every move or resize updates the state in memory (cheap); the file
//!   is written when the maximised state changes, when the window loses the focus, when it closes
//!   and at quit, only if something changed since the last write. A crash loses at most the moves
//!   made since the window last lost the focus.

use std::path::{Path, PathBuf};

use gpui::{App, AppContext as _, Bounds, Pixels, Point, Size, Subscription, Window, WindowBounds, point, px, size};
use serde::{Deserialize, Serialize};

pub const FILE: &str = "window-state.json";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// The window's bounds as GPUI reports them (`Window::window_bounds`: the restored bounds of a
    /// maximised window), logical pixels, the client-side margin included where there is one.
    pub width: f32,
    pub height: f32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f32>,
    #[serde(default)]
    pub maximized: bool,
}

/// Whether this platform lets the application place its window.
pub fn positions_windows() -> bool {
    if cfg!(any(target_os = "linux", target_os = "freebsd")) {
        // GPUI's choice of backend: Wayland whenever WAYLAND_DISPLAY is set.
        std::env::var_os("WAYLAND_DISPLAY").is_none_or(|d| d.is_empty())
    } else {
        true
    }
}

pub fn load(profile: &Path) -> Option<Saved> {
    let text = std::fs::read_to_string(profile.join(FILE)).ok()?;
    match serde_json::from_str::<Saved>(&text) {
        Ok(saved) if saved.width.is_finite() && saved.height.is_finite() && saved.width > 0. && saved.height > 0. => Some(saved),
        _ => {
            log::warn!("[window] ignoring an unreadable {FILE}");
            None
        }
    }
}

fn store(profile: &Path, saved: &Saved) {
    let path = profile.join(FILE);
    let temporary = profile.join(format!("{FILE}.tmp"));
    let result = serde_json::to_vec_pretty(saved)
        .map_err(std::io::Error::other)
        .and_then(|bytes| std::fs::write(&temporary, bytes))
        .and_then(|()| std::fs::rename(&temporary, &path));
    if let Err(e) = result {
        log::warn!("[window] couldn't save {}: {e}", path.display());
    }
}

/// Where and how large the window opens, from what was saved: `origin` is `None` when the saved
/// place is not to be used (Wayland, nothing saved, off every screen).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Restored {
    pub origin: Option<Point<Pixels>>,
    pub size: Size<Pixels>,
    pub maximized: bool,
}

/// The saved state made to fit what is connected now. `displays` are the screens' bounds;
/// `minimum` is an outer size (the margin included); `place`: the platform places windows for the
/// application ([`positions_windows`]).
pub fn fit(saved: Saved, displays: &[Bounds<Pixels>], minimum: Size<Pixels>, place: bool) -> Restored {
    let mut wanted = size(px(saved.width.max(f32::from(minimum.width))), px(saved.height.max(f32::from(minimum.height))));
    // Never larger than the largest screen.
    if let Some(largest) = displays.iter().max_by(|a, b| {
        (f32::from(a.size.width) * f32::from(a.size.height)).total_cmp(&(f32::from(b.size.width) * f32::from(b.size.height)))
    }) {
        wanted.width = wanted.width.min(largest.size.width.max(minimum.width));
        wanted.height = wanted.height.min(largest.size.height.max(minimum.height));
    }
    let origin = match (saved.x, saved.y) {
        (Some(x), Some(y)) if place => {
            let bounds = Bounds::new(point(px(x), px(y)), wanted);
            on_screen(bounds, displays).then_some(bounds.origin)
        }
        _ => None,
    };
    Restored { origin, size: wanted, maximized: saved.maximized }
}

/// The bounds to open the main window with: the profile's saved state fitted to the screens, else
/// `default` centred (both outer sizes).
pub fn initial_bounds(profile: Option<&Path>, default: Size<Pixels>, minimum: Size<Pixels>, cx: &App) -> WindowBounds {
    let Some(saved) = profile.and_then(load) else {
        return WindowBounds::Windowed(Bounds::centered(None, default, cx));
    };
    let displays: Vec<Bounds<Pixels>> = cx.displays().iter().map(|d| d.bounds()).collect();
    let restored = fit(saved, &displays, minimum, positions_windows());
    let bounds = match restored.origin {
        Some(origin) => Bounds::new(origin, restored.size),
        None => Bounds::centered(None, restored.size, cx),
    };
    if restored.maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) }
}

/// At least a 64 × 64 px piece of the window (its title bar's end, typically) is on some screen.
fn on_screen(bounds: Bounds<Pixels>, displays: &[Bounds<Pixels>]) -> bool {
    displays.iter().any(|display| {
        let left = bounds.origin.x.max(display.origin.x);
        let top = bounds.origin.y.max(display.origin.y);
        let right = (bounds.origin.x + bounds.size.width).min(display.origin.x + display.size.width);
        let bottom = (bounds.origin.y + bounds.size.height).min(display.origin.y + display.size.height);
        right - left >= px(64.) && bottom - top >= px(64.)
    })
}

/// The window's state as it is saved: Wayland reports the bounds a window opens with
/// (`window_bounds`, the client-side margin included); X11 opens a window at the size it is given
/// and reports it with the margin around it, so there the bounds without it are what reopens the
/// same window (`inner_window_bounds`; measured under XWayland on KWin: saving `window_bounds`
/// grew the window by 20 px and moved it by 10 px at every start).
fn current(window: &Window, place: bool) -> Option<Saved> {
    let reported = if place { window.inner_window_bounds() } else { window.window_bounds() };
    let (bounds, maximized) = match reported {
        WindowBounds::Windowed(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) => (bounds, true),
        // Fullscreen is a stream's state, not a window to come back to: what it came from stays.
        WindowBounds::Fullscreen(_) => return None,
    };
    Some(Saved {
        width: f32::from(bounds.size.width),
        height: f32::from(bounds.size.height),
        x: place.then(|| f32::from(bounds.origin.x)),
        y: place.then(|| f32::from(bounds.origin.y)),
        maximized,
    })
}

/// What would be saved for `window` now (the self-tests compare a reopened window with it).
pub fn current_state(window: &Window) -> Option<Saved> {
    current(window, positions_windows())
}

/// What [`track`] keeps between events (an entity: GPUI's window observers belong to one).
pub struct Tracker {
    profile: PathBuf,
    place: bool,
    latest: Option<Saved>,
    written: Option<Saved>,
    /// X11: GPUI asks to maximise before the window is mapped, which the window manager ignores
    /// (`_NET_WM_STATE` client messages count only for mapped windows): asked again once the
    /// window is active.
    maximize_when_active: bool,
    _subscriptions: Vec<Subscription>,
}

impl Tracker {
    fn write_if_changed(&mut self) {
        if let Some(latest) = self.latest
            && self.written != Some(latest)
        {
            store(&self.profile, &latest);
            self.written = Some(latest);
            crate::marker(format_args!(
                "window-state saved {}x{}{}",
                latest.width,
                latest.height,
                if latest.maximized { " maximized" } else { "" }
            ));
        }
    }

    fn moved(&mut self, window: &Window) {
        let Some(mut now) = current(window, self.place) else { return };
        let before = self.latest.or(self.written);
        // A maximised window keeps the size and place it had before (X11 reports the maximised
        // bounds themselves).
        if now.maximized
            && let Some(before) = before
        {
            now = Saved { maximized: true, ..before };
        }
        self.latest = Some(now);
        if before.is_some_and(|b| b.maximized != now.maximized) {
            self.write_if_changed();
        }
    }
}

/// Follows the window's bounds and writes them as described in the module doc. Keep the entity as
/// long as the window.
pub fn track(window: &mut Window, profile: PathBuf, cx: &mut App) -> gpui::Entity<Tracker> {
    let place = positions_windows();
    let written = load(&profile);
    let id = window.window_handle().window_id();
    cx.new(|cx: &mut gpui::Context<Tracker>| {
        let bounds = cx.observe_window_bounds(window, |this, window, _| this.moved(window));
        let activation = cx.observe_window_activation(window, |this, window, _| {
            if !window.is_window_active() {
                this.write_if_changed();
            } else if std::mem::take(&mut this.maximize_when_active) && !window.is_maximized() {
                window.zoom_window();
            }
        });
        let quitting = cx.on_app_quit(|this, _| {
            this.write_if_changed();
            async {}
        });
        let weak = cx.weak_entity();
        let closing = cx.on_window_closed(move |cx, closed| {
            if closed == id {
                weak.update(cx, |this, _| this.write_if_changed()).ok();
            }
        });
        let maximize_when_active = place && cfg!(any(target_os = "linux", target_os = "freebsd")) && written.is_some_and(|w| w.maximized);
        let mut tracker = Tracker { profile, place, latest: None, written, maximize_when_active, _subscriptions: vec![bounds, activation, quitting, closing] };
        tracker.moved(window);
        tracker
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
    }

    const MIN: Size<Pixels> = Size { width: px(820.), height: px(620.) };

    #[test]
    fn a_window_comes_back_where_it_was_if_that_is_on_a_screen() {
        let screens = [display(0., 0., 2560., 1440.), display(2560., 0., 1920., 1080.)];
        let saved = Saved { width: 1000., height: 700., x: Some(2700.), y: Some(100.), maximized: false };
        let back = fit(saved, &screens, MIN, true);
        assert_eq!(back, Restored { origin: Some(point(px(2700.), px(100.))), size: size(px(1000.), px(700.)), maximized: false });
        // The second monitor is gone: no place, same size.
        let gone = fit(saved, &screens[..1], MIN, true);
        assert_eq!(gone, Restored { origin: None, size: size(px(1000.), px(700.)), maximized: false });
        // Wayland: never a place.
        let gone = fit(saved, &screens, MIN, false);
        assert_eq!(gone, Restored { origin: None, size: size(px(1000.), px(700.)), maximized: false });
    }

    #[test]
    fn sizes_stay_between_the_minimum_and_the_largest_screen() {
        let screens = [display(0., 0., 1920., 1080.)];
        let tiny = Saved { width: 300., height: 200., x: None, y: None, maximized: false };
        assert_eq!(fit(tiny, &screens, MIN, true), Restored { origin: None, size: MIN, maximized: false });
        let huge = Saved { width: 5000., height: 3000., x: None, y: None, maximized: true };
        assert_eq!(fit(huge, &screens, MIN, true), Restored { origin: None, size: size(px(1920.), px(1080.)), maximized: true });
    }

    #[test]
    fn the_file_round_trips_and_bad_ones_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let saved = Saved { width: 1111., height: 777., x: None, y: None, maximized: true };
        store(dir.path(), &saved);
        assert_eq!(load(dir.path()), Some(saved));
        std::fs::write(dir.path().join(FILE), "{\"width\": -1, \"height\": 2}").unwrap();
        assert_eq!(load(dir.path()), None);
        std::fs::write(dir.path().join(FILE), "nonsense").unwrap();
        assert_eq!(load(dir.path()), None);
    }
}
