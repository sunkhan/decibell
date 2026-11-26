//! The main window's size, place and maximised state, per profile (`window-state.json`), restored
//! at start: Electron's window opened at 1280 × 800 every time (`window.ts`); here the last run's
//! window comes back, at least 800 × 600.
//!
//! - **Position** only where the platform lets an application place its window: X11 and Windows.
//!   Wayland's compositor places windows itself: no position is saved or asked for there.
//! - **Which bounds**: what reopens the same window (`current`): Wayland's `window_bounds`, X11's
//!   `inner_window_bounds`. A maximised window keeps the windowed size it came from.
//! - **X11 (KWin) and the client-side margin**: KWin learns the margin (`_GTK_FRAME_EXTENTS`) only
//!   after the window is mapped, and depending on the order the window comes up (or back from
//!   maximised) at its size or 20 px smaller. Corrected once: the saved size when the window first
//!   becomes active, the size it had when it comes back from maximised (each a single `resize`, no
//!   timer); without that the window shrank by 20 px on some starts, and kept shrinking.
//! - **Off every screen** (a monitor that is gone): the saved place is dropped and the window opens
//!   centred, as on a first start; a size larger than every screen is shrunk to the largest.
//! - **Saving without a timer**: every move or resize updates the state in memory (cheap); the file
//!   is written when the maximised state changes, when the window loses the focus, when it closes
//!   and at quit, only if something changed since the last write. A crash loses at most the moves
//!   made since the window last lost the focus.

use std::path::{Path, PathBuf};

use gpui::{App, AppContext as _, Bounds, DisplayId, Pixels, Point, Size, Subscription, Window, WindowBounds, point, px, size};
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
    /// The screen the saved place is on (an index into the `displays` given to [`fit`]).
    pub display: Option<usize>,
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
    let placed = match (saved.x, saved.y) {
        (Some(x), Some(y)) if place => {
            let bounds = Bounds::new(point(px(x), px(y)), wanted);
            screen_of(bounds, displays).map(|display| (bounds.origin, display))
        }
        _ => None,
    };
    Restored { origin: placed.map(|p| p.0), display: placed.map(|p| p.1), size: wanted, maximized: saved.maximized }
}

/// The bounds to open the main window with: the profile's saved state fitted to the screens, else
/// `default` centred (both outer sizes).
/// The bounds to open the main window with and the screen to open it on: the profile's saved state
/// fitted to the screens, else `default` centred (both outer sizes). The screen matters on Windows:
/// GPUI places a window on the screen it is given (the primary one otherwise) and converts the
/// bounds with that screen's DPI scale, dropping bounds whose centre is on another screen.
pub fn initial_bounds(profile: Option<&Path>, default: Size<Pixels>, minimum: Size<Pixels>, cx: &App) -> (WindowBounds, Option<DisplayId>) {
    let Some(saved) = profile.and_then(load) else {
        return (WindowBounds::Windowed(Bounds::centered(None, default, cx)), None);
    };
    let displays = cx.displays();
    let screens: Vec<Bounds<Pixels>> = displays.iter().map(|d| d.bounds()).collect();
    let restored = fit(saved, &screens, minimum, positions_windows());
    let display = restored.display.and_then(|i| displays.get(i)).map(|d| d.id());
    let bounds = match restored.origin {
        Some(origin) => Bounds::new(origin, restored.size),
        None => Bounds::centered(None, restored.size, cx),
    };
    let bounds = if restored.maximized { WindowBounds::Maximized(bounds) } else { WindowBounds::Windowed(bounds) };
    (bounds, display)
}

/// The screen the window's centre is on (each screen's bounds in its own logical pixels, as GPUI
/// reports them; on Windows a window's saved bounds are in the logical pixels of the screen it was
/// on), if any: a place on no screen (a monitor that is gone) is dropped.
fn screen_of(bounds: Bounds<Pixels>, displays: &[Bounds<Pixels>]) -> Option<usize> {
    let center = bounds.center();
    displays.iter().position(|display| display.contains(&center))
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
    /// X11: the visible size a restored window must have; checked once the window is active,
    /// because KWin can map it 20 px smaller (it learns the client-side margin only after the
    /// window is mapped: `HANDOFF.md` §5).
    size_when_active: Option<Size<Pixels>>,
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

    fn moved(&mut self, window: &mut Window) {
        let Some(mut now) = current(window, self.place) else { return };
        let before = self.latest.or(self.written);
        // A maximised window keeps the size and place it had before (X11 reports the maximised
        // bounds themselves).
        if now.maximized
            && let Some(before) = before
        {
            now = Saved { maximized: true, ..before };
        }
        // X11 (KWin): coming back from maximised, the window can land 20 px off the size it had
        // (the client-side margin): put it back to that size once.
        if self.place
            && cfg!(any(target_os = "linux", target_os = "freebsd"))
            && let Some(before) = before
            && before.maximized
            && !now.maximized
            && ((now.width - before.width).abs() > 0. || (now.height - before.height).abs() > 0.)
            && (now.width - before.width).abs() <= 20.
            && (now.height - before.height).abs() <= 20.
        {
            window.resize(decibell_ui::outer_window_size(size(px(before.width), px(before.height))));
            now.width = before.width;
            now.height = before.height;
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
            } else {
                if std::mem::take(&mut this.maximize_when_active) && !window.is_maximized() {
                    window.zoom_window();
                }
                if let Some(wanted) = this.size_when_active.take()
                    && !window.is_maximized()
                    && let WindowBounds::Windowed(inner) = window.inner_window_bounds()
                    && inner.size != wanted
                {
                    window.resize(decibell_ui::outer_window_size(wanted));
                }
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
        let x11 = place && cfg!(any(target_os = "linux", target_os = "freebsd"));
        let maximize_when_active = x11 && written.is_some_and(|w| w.maximized);
        let size_when_active = written.filter(|w| x11 && !w.maximized).map(|w| size(px(w.width), px(w.height)));
        let mut tracker = Tracker {
            profile,
            place,
            latest: None,
            written,
            maximize_when_active,
            size_when_active,
            _subscriptions: vec![bounds, activation, quitting, closing],
        };
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
        assert_eq!(back, Restored { origin: Some(point(px(2700.), px(100.))), display: Some(1), size: size(px(1000.), px(700.)), maximized: false });
        // The second monitor is gone: no place, same size.
        let gone = fit(saved, &screens[..1], MIN, true);
        assert_eq!(gone, Restored { origin: None, display: None, size: size(px(1000.), px(700.)), maximized: false });
        // Wayland: never a place.
        let gone = fit(saved, &screens, MIN, false);
        assert_eq!(gone, Restored { origin: None, display: None, size: size(px(1000.), px(700.)), maximized: false });
    }

    #[test]
    fn sizes_stay_between_the_minimum_and_the_largest_screen() {
        let screens = [display(0., 0., 1920., 1080.)];
        let tiny = Saved { width: 300., height: 200., x: None, y: None, maximized: false };
        assert_eq!(fit(tiny, &screens, MIN, true), Restored { origin: None, display: None, size: MIN, maximized: false });
        let huge = Saved { width: 5000., height: 3000., x: None, y: None, maximized: true };
        assert_eq!(fit(huge, &screens, MIN, true), Restored { origin: None, display: None, size: size(px(1920.), px(1080.)), maximized: true });
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
