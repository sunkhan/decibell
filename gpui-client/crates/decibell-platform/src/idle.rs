//! The idle source behind auto-away (Electron: `powerMonitor.getSystemIdleTime` / `getSystemIdleState`
//! in `idle.ts`), chosen by the display server GPUI runs on:
//!
//! - **Wayland** (GPUI's choice whenever `WAYLAND_DISPLAY` is set and not empty) and GNOME: the core's
//!   `system_idle` (the compositor's `ext-idle-notify`, GNOME's IdleMonitor).
//! - **X11** (an X11 session, or XWayland when the client is started with `WAYLAND_DISPLAY` unset or
//!   empty): [`x11`], the X server's `IDLETIME` counter through SYNC alarms (a thread woken only at the
//!   threshold and at the next input), MIT-SCREEN-SAVER's `QueryInfo` on a server without them.
//! - **Windows**: `GetLastInputInfo`, which knows how long the session has been idle, so `AutoAway`
//!   arms its one timer for exactly when the threshold can be reached.
//! - macOS: the core's answer ("unsupported": Away only on lock / sleep).
//!
//! Lock and sleep come from [`crate::session`].

use std::rc::Rc;

use decibell_model::Model;
use decibell_model::live::presence::IdleSource;
use gpui::App;

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub(crate) mod x11;

/// The source the app gives `AutoAway` on this platform. Call once, with the model.
pub fn source(model: &Model, cx: &mut App) -> Rc<dyn IdleSource> {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        if cx.compositor_name() == "X11" {
            crate::marker(format_args!("idle source x11"));
            return x11_source(model, cx);
        }
    }
    let _ = (model, &cx);
    #[cfg(windows)]
    let (name, source): (_, Rc<dyn IdleSource>) = ("windows", Rc::new(last_input::LastInput));
    #[cfg(not(windows))]
    let (name, source): (_, Rc<dyn IdleSource>) = ("core", Rc::new(decibell_model::actions::presence::CoreIdleSource));
    crate::marker(format_args!("idle source {name}"));
    source
}

/// The X11 source on the server GPUI uses, its edges turned into `AutoAway::poke` on the UI thread
/// (one wake each), and switched off with auto-away itself: the threshold at 0, or no statuses at
/// central (before sign-in, after sign-out). Those are the two inputs `decibell_model::live::install`
/// configures `AutoAway` from; `AutoAway` itself only reads while on, which is what turns it on.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn x11_source(model: &Model, cx: &mut App) -> Rc<dyn IdleSource> {
    use futures::StreamExt as _;

    let (tx, mut edges) = futures::channel::mpsc::unbounded::<x11::Edge>();
    let source = Rc::new(x11::X11Idle::new(
        None,
        cx.background_executor().clone(),
        std::sync::Arc::new(move |edge| {
            let _ = tx.unbounded_send(edge);
        }),
    ));
    let away = model.live.presence.auto_away.downgrade();
    cx.spawn(async move |cx| {
        while let Some(edge) = edges.next().await {
            crate::marker(format_args!("idle x11 {edge}"));
            let Some(away) = away.upgrade() else { break };
            cx.update(|cx| away.update(cx, |a, cx| a.poke(cx)));
        }
    })
    .detach();
    let follow = {
        let (model, source) = (model.clone(), source.clone());
        move |cx: &mut App| {
            let on = model.live.presence.own.read(cx).supported && model.settings.read(cx).get().away_after_minutes > 0;
            if !on {
                source.disable();
            }
        }
    };
    cx.observe(&model.settings, {
        let follow = follow.clone();
        move |_, cx| follow(cx)
    })
    .detach();
    cx.observe(&model.live.presence.own, move |_, cx| follow(cx)).detach();
    source
}

#[cfg(windows)]
mod last_input {
    use std::time::Duration;

    use decibell_model::live::presence::{IdleReading, IdleSource};
    use futures::future::LocalBoxFuture;

    /// The session's last keyboard / mouse input (`GetLastInputInfo`): a cheap call, made when
    /// `AutoAway` asks (once per threshold while active, every second while Away, as Electron).
    pub struct LastInput;

    impl IdleSource for LastInput {
        fn read(&self, threshold: Duration) -> LocalBoxFuture<'static, IdleReading> {
            let reading = match crate::win32::idle_for() {
                Some(idle) if idle >= threshold => IdleReading::Idle,
                Some(idle) => IdleReading::Active { idle_for: Some(idle) },
                None => IdleReading::Unsupported,
            };
            Box::pin(async move { reading })
        }

        fn pushes_resume(&self) -> bool {
            false
        }
    }
}
