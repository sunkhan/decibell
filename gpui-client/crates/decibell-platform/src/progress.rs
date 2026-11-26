//! The transfers' progress on the taskbar / launcher (`window.ts`' `setTransferProgress` →
//! `BrowserWindow.setProgressBar`), from `model.live.transfers` as `TransfersButton.tsx` computes
//! it (`Transfers::progress`): the fraction over everything running, paused when nothing runs, none
//! when nothing is left.
//!
//! - Linux: the `com.canonical.Unity.LauncherEntry.Update` D-Bus signal (what Electron sends on
//!   Linux; KDE's task manager and Unity-style docks show it), keyed by the desktop file
//!   [`DESKTOP_ID`]. The session connection exists only while a progress shows: made on the first
//!   value, dropped after the clear.
//! - Windows: `ITaskbarList3` (normal / paused / none).
//!
//! Sent only when what shows changes (to the 1/1000), at most four times a second while it moves
//! (the renderer's rate), always at once when it appears, pauses or clears. No timer: the store
//! notifies on every progress tick, and the last value of a burst is a clear or a pause, which
//! always goes out.

use std::time::{Duration, Instant};

use decibell_model::Model;
use gpui::{AnyWindowHandle, App, AppContext as _, Subscription};

/// The desktop file the launcher entry belongs to (`application://<id>`): the app id the window
/// carries (Wayland `app_id`, X11 `WM_CLASS`) with `.desktop`.
pub const DESKTOP_ID: &str = "decibell.desktop";
/// The object path the signal is sent from (any path: listeners match the interface).
pub const OBJECT_PATH: &str = "/com/canonical/unity/launcherentry/decibell";
/// Fastest repeat of a moving value (the renderer sent at most four a second).
const MIN_INTERVAL: Duration = Duration::from_millis(250);

/// What the taskbar shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    None,
    /// The fraction in thousandths, and whether it shows paused.
    Value { permille: u16, paused: bool },
}

impl Shown {
    pub fn of(progress: &decibell_model::live::transfers::TransferProgress) -> Shown {
        match progress.fraction {
            None => Shown::None,
            Some(fraction) => Shown::Value {
                permille: (fraction.clamp(0.0, 1.0) * 1000.0).round() as u16,
                paused: progress.paused_only,
            },
        }
    }
}

/// Decides what to send: the change rules above, testable without a desktop.
#[derive(Default)]
pub struct Throttle {
    sent: Option<Shown>,
    at: Option<Instant>,
}

impl Throttle {
    /// `Some(shown)` when `shown` should go out now.
    pub fn next(&mut self, shown: Shown, now: Instant) -> Option<Shown> {
        let previous = self.sent.unwrap_or(Shown::None);
        if previous == shown {
            return None;
        }
        let moving_only = matches!(
            (previous, shown),
            (Shown::Value { paused: a, .. }, Shown::Value { paused: b, .. }) if a == b
        );
        if moving_only && self.at.is_some_and(|at| now.duration_since(at) < MIN_INTERVAL) {
            return None;
        }
        self.sent = Some(shown);
        self.at = Some(now);
        Some(shown)
    }
}

pub(crate) fn install(window: AnyWindowHandle, cx: &mut App) -> Option<Subscription> {
    let model = cx.try_global::<Model>()?.clone();
    let mut throttle = Throttle::default();
    let mut sink = Sink::default();
    Some(cx.observe(&model.live.transfers, move |transfers, cx| {
        let shown = Shown::of(&transfers.read(cx).progress());
        if let Some(shown) = throttle.next(shown, Instant::now()) {
            crate::marker(format_args!("progress {}", describe(shown)));
            sink.show(shown, window, cx);
        }
    }))
}

fn describe(shown: Shown) -> String {
    match shown {
        Shown::None => "none".into(),
        Shown::Value { permille, paused } => format!("{permille}{}", if paused { " paused" } else { "" }),
    }
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
#[derive(Default)]
struct Sink {
    /// Updates go through one task, in order (a value overtaking the clear would leave a bar).
    queue: Option<futures::channel::mpsc::UnboundedSender<Shown>>,
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
impl Sink {
    fn show(&mut self, shown: Shown, _: AnyWindowHandle, cx: &mut App) {
        if self.queue.is_none() {
            let (tx, rx) = futures::channel::mpsc::unbounded();
            cx.background_spawn(launcher_entry(rx)).detach();
            self.queue = Some(tx);
        }
        if let Some(queue) = &self.queue {
            queue.unbounded_send(shown).ok();
        }
    }
}

/// Sends the launcher entry's updates; the connection lives from a value to its clear.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
async fn launcher_entry(mut rx: futures::channel::mpsc::UnboundedReceiver<Shown>) {
    use futures::StreamExt as _;
    let mut connection: Option<zbus::Connection> = None;
    while let Some(shown) = rx.next().await {
        if connection.is_none() {
            if shown == Shown::None {
                continue;
            }
            match zbus::Connection::session().await {
                Ok(c) => connection = Some(c),
                Err(e) => {
                    log::debug!("[progress] no session bus: {e}");
                    continue;
                }
            }
        }
        let Some(c) = &connection else { continue };
        if let Err(e) = emit(c, shown).await {
            log::debug!("[progress] LauncherEntry.Update: {e}");
        }
        if shown == Shown::None {
            connection = None;
        }
    }
}

/// One `Update(s app_uri, a{sv} properties)`.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
async fn emit(connection: &zbus::Connection, shown: Shown) -> zbus::Result<()> {
    use std::collections::HashMap;
    use zbus::zvariant::Value;
    let mut properties: HashMap<&str, Value> = HashMap::new();
    match shown {
        Shown::None => {
            properties.insert("progress-visible", Value::from(false));
            properties.insert("progress", Value::from(0.0f64));
        }
        Shown::Value { permille, .. } => {
            // The protocol has no paused state: Electron shows the value either way on Linux.
            properties.insert("progress-visible", Value::from(true));
            properties.insert("progress", Value::from(f64::from(permille) / 1000.0));
        }
    }
    let app_uri = format!("application://{DESKTOP_ID}");
    connection
        .emit_signal(None::<&str>, OBJECT_PATH, "com.canonical.Unity.LauncherEntry", "Update", &(app_uri, properties))
        .await
}

#[cfg(windows)]
#[derive(Default)]
struct Sink {
    taskbar: crate::win32::TaskbarProgress,
}

#[cfg(windows)]
impl Sink {
    fn show(&mut self, shown: Shown, window: AnyWindowHandle, cx: &mut App) {
        let value = match shown {
            Shown::None => None,
            Shown::Value { permille, paused } => Some((f64::from(permille) / 1000.0, paused)),
        };
        let taskbar = &mut self.taskbar;
        let _ = window.update(cx, |_, window, _| taskbar.set(window, value));
    }
}

/// macOS (the dock tile's progress) and others: not built.
#[cfg(not(any(target_os = "linux", target_os = "freebsd", windows)))]
#[derive(Default)]
struct Sink;

#[cfg(not(any(target_os = "linux", target_os = "freebsd", windows)))]
impl Sink {
    fn show(&mut self, _: Shown, _: AnyWindowHandle, _: &mut App) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sends_changes_at_most_four_times_a_second_and_never_drops_a_clear() {
        let mut t = Throttle::default();
        let t0 = Instant::now();
        let value = |p| Shown::Value { permille: p, paused: false };
        assert_eq!(t.next(Shown::None, t0), None, "nothing to clear at first");
        assert_eq!(t.next(value(100), t0), Some(value(100)), "appears at once");
        assert_eq!(t.next(value(100), t0 + Duration::from_millis(300)), None, "unchanged");
        assert_eq!(t.next(value(150), t0 + Duration::from_millis(100)), None, "too soon");
        assert_eq!(t.next(value(200), t0 + Duration::from_millis(260)), Some(value(200)));
        let paused = Shown::Value { permille: 200, paused: true };
        assert_eq!(t.next(paused, t0 + Duration::from_millis(270)), Some(paused), "a pause goes out at once");
        assert_eq!(t.next(Shown::None, t0 + Duration::from_millis(271)), Some(Shown::None), "the clear goes out at once");
        assert_eq!(t.next(Shown::None, t0 + Duration::from_millis(900)), None);
    }

    #[test]
    fn shown_follows_the_store() {
        use decibell_model::live::transfers::TransferProgress;
        let none = TransferProgress { active: 0, paused: 0, uploads: 0, fraction: None, paused_only: false };
        assert_eq!(Shown::of(&none), Shown::None);
        let half = TransferProgress { active: 1, paused: 0, uploads: 0, fraction: Some(0.5), paused_only: false };
        assert_eq!(Shown::of(&half), Shown::Value { permille: 500, paused: false });
        let paused = TransferProgress { active: 0, paused: 1, uploads: 1, fraction: Some(0.25), paused_only: true };
        assert_eq!(Shown::of(&paused), Shown::Value { permille: 250, paused: true });
    }
}
