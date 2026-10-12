//! Auto-away on Linux (docs/superpowers/specs/2026-10-11-user-status-design.md).
//!
//! Electron's powerMonitor reads no idle time on a Wayland session: on KDE
//! Plasma it reports 0 s / "active" forever, natively and through XWayland
//! alike (measured 2026-10-12), and KDE's org.freedesktop.ScreenSaver
//! answers GetSessionIdleTime with "not supported on this platform". So:
//!
//! - Wayland: the compositor announces it. ext-idle-notify-v1 (KWin, Sway,
//!   Hyprland, niri, …) sends `idled` after the threshold without input and
//!   `resumed` on the next input. Version 2's input-idle notification is used
//!   when offered, so a playing video elsewhere doesn't keep the user online.
//! - GNOME (Mutter has no ext-idle-notify): org.gnome.Mutter.IdleMonitor.
//! - Screen lock: org.freedesktop.ScreenSaver / org.gnome.ScreenSaver
//!   GetActive (powerMonitor's lock events are macOS / Windows only).
//!
//! X11 sessions, Windows and macOS answer "unsupported" and the renderer
//! falls back to powerMonitor, which works there.

/// "idle" | "active" | "locked" | "unsupported" for a threshold in seconds.
#[cfg(not(target_os = "linux"))]
pub fn state(_threshold_secs: u32) -> &'static str {
    "unsupported"
}

#[cfg(target_os = "linux")]
pub use linux::state;

#[cfg(target_os = "linux")]
mod linux {
    use std::os::fd::AsRawFd;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex, OnceLock};
    use std::time::Duration;

    use wayland_client::globals::{registry_queue_init, GlobalListContents};
    use wayland_client::protocol::{wl_registry, wl_seat};
    use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
    use wayland_protocols::ext::idle_notify::v1::client::{
        ext_idle_notification_v1::{self, ExtIdleNotificationV1},
        ext_idle_notifier_v1::ExtIdleNotifierV1,
    };

    pub fn state(threshold_secs: u32) -> &'static str {
        if screen_locked() {
            return "locked";
        }
        if std::env::var_os("WAYLAND_DISPLAY").is_none() {
            return "unsupported";
        }
        if let Some(idle) = wayland_idle(threshold_secs) {
            return if idle { "idle" } else { "active" };
        }
        match mutter_idle_ms() {
            Some(ms) if ms >= u64::from(threshold_secs) * 1000 => "idle",
            Some(_) => "active",
            None => "unsupported",
        }
    }

    // ── Screen lock / GNOME idle over D-Bus ────────────────────────────

    fn session_bus() -> Option<&'static zbus::blocking::Connection> {
        static BUS: OnceLock<Option<zbus::blocking::Connection>> = OnceLock::new();
        BUS.get_or_init(|| zbus::blocking::Connection::session().ok()).as_ref()
    }

    fn screen_locked() -> bool {
        let Some(bus) = session_bus() else { return false };
        for (dest, path) in [
            ("org.freedesktop.ScreenSaver", "/ScreenSaver"),
            ("org.gnome.ScreenSaver", "/org/gnome/ScreenSaver"),
        ] {
            if let Ok(reply) = bus.call_method(Some(dest), path, Some(dest), "GetActive", &()) {
                if let Ok(active) = reply.body().deserialize::<bool>() {
                    return active;
                }
            }
        }
        false
    }

    fn mutter_idle_ms() -> Option<u64> {
        let reply = session_bus()?
            .call_method(
                Some("org.gnome.Mutter.IdleMonitor"),
                "/org/gnome/Mutter/IdleMonitor/Core",
                Some("org.gnome.Mutter.IdleMonitor"),
                "GetIdletime",
                &(),
            )
            .ok()?;
        reply.body().deserialize::<u64>().ok()
    }

    // ── Wayland ext-idle-notify watcher ────────────────────────────────

    /// One watcher thread per threshold; replaced when the threshold changes.
    struct Watch {
        threshold_secs: u32,
        idle: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
    }

    static WATCH: Mutex<Option<Watch>> = Mutex::new(None);
    /// The compositor offers no ext-idle-notify (e.g. GNOME): stop trying.
    static NO_NOTIFIER: AtomicBool = AtomicBool::new(false);

    fn wayland_idle(threshold_secs: u32) -> Option<bool> {
        if NO_NOTIFIER.load(Ordering::Relaxed) {
            return None;
        }
        let mut watch = WATCH.lock().ok()?;
        if let Some(w) = watch.as_ref() {
            if w.threshold_secs == threshold_secs && !w.stop.load(Ordering::Relaxed) {
                return Some(w.idle.load(Ordering::Relaxed));
            }
            w.stop.store(true, Ordering::Relaxed);
        }
        *watch = None;

        let idle = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<bool>();
        {
            let (idle, stop) = (idle.clone(), stop.clone());
            std::thread::Builder::new()
                .name("system-idle".into())
                .spawn(move || run(threshold_secs, idle, stop.clone(), ready_tx))
                .ok()?;
        }
        match ready_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(true) => {
                *watch = Some(Watch { threshold_secs, idle: idle.clone(), stop });
                Some(idle.load(Ordering::Relaxed))
            }
            Ok(false) => {
                NO_NOTIFIER.store(true, Ordering::Relaxed);
                None
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                None
            }
        }
    }

    struct IdleState {
        idle: Arc<AtomicBool>,
    }

    fn run(
        threshold_secs: u32,
        idle: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
        ready: mpsc::Sender<bool>,
    ) {
        let setup = || -> Result<_, String> {
            let conn = Connection::connect_to_env().map_err(|e| format!("connect: {e}"))?;
            let (globals, queue) = registry_queue_init::<IdleState>(&conn)
                .map_err(|e| format!("registry: {e}"))?;
            let qh = queue.handle();
            let notifier: ExtIdleNotifierV1 = globals
                .bind(&qh, 1..=2, ())
                .map_err(|e| format!("ext_idle_notifier_v1: {e}"))?;
            let seat: wl_seat::WlSeat =
                globals.bind(&qh, 1..=1, ()).map_err(|e| format!("wl_seat: {e}"))?;
            let timeout_ms = threshold_secs.saturating_mul(1000);
            let notification = if notifier.version() >= 2 {
                notifier.get_input_idle_notification(timeout_ms, &seat, &qh, ())
            } else {
                notifier.get_idle_notification(timeout_ms, &seat, &qh, ())
            };
            Ok((conn, queue, notifier, seat, notification))
        };
        let (conn, mut queue, notifier, seat, notification) = match setup() {
            Ok(v) => v,
            Err(e) => {
                log::info!("[system-idle] no Wayland idle source: {e}");
                let _ = ready.send(false);
                return;
            }
        };
        let mut st = IdleState { idle };
        let _ = ready.send(true);

        // Poll with a timeout instead of blocking_dispatch, so a threshold
        // change (stop) ends this thread within a second.
        while !stop.load(Ordering::Relaxed) {
            if queue.dispatch_pending(&mut st).is_err() || conn.flush().is_err() {
                break;
            }
            let Some(guard) = queue.prepare_read() else { continue };
            let mut pfd = libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd for the duration of the call.
            let ready_fds = unsafe { libc::poll(&mut pfd, 1, 1000) };
            if ready_fds > 0 {
                if guard.read().is_err() {
                    break;
                }
            } else {
                drop(guard);
            }
        }
        notification.destroy();
        notifier.destroy();
        if seat.version() >= 5 {
            seat.release();
        }
        let _ = conn.flush();
        stop.store(true, Ordering::Relaxed);
    }

    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for IdleState {
        fn event(
            _: &mut Self,
            _: &wl_registry::WlRegistry,
            _: wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<wl_seat::WlSeat, ()> for IdleState {
        fn event(
            _: &mut Self,
            _: &wl_seat::WlSeat,
            _: wl_seat::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ExtIdleNotifierV1, ()> for IdleState {
        fn event(
            _: &mut Self,
            _: &ExtIdleNotifierV1,
            _: <ExtIdleNotifierV1 as Proxy>::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }

    impl Dispatch<ExtIdleNotificationV1, ()> for IdleState {
        fn event(
            state: &mut Self,
            _: &ExtIdleNotificationV1,
            event: ext_idle_notification_v1::Event,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                ext_idle_notification_v1::Event::Idled => state.idle.store(true, Ordering::Relaxed),
                ext_idle_notification_v1::Event::Resumed => state.idle.store(false, Ordering::Relaxed),
                _ => {}
            }
        }
    }
}
