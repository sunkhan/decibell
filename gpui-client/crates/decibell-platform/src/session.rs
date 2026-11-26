//! Sleep and screen lock make us Away at once (`electron/main/idle.ts`: `powerMonitor`'s `suspend`
//! / `resume` and `lock-screen` / `unlock-screen` → the renderer's `useAutoAway`), through the
//! model's `AutoAway::set_locked` (which the model leaves to the app).
//!
//! - Sleep / wake: GPUI's `on_system_sleep` / `on_system_wake` (logind's `PrepareForSleep` on
//!   Linux, `WM_POWERBROADCAST` on Windows), subscriptions GPUI already holds.
//! - Screen lock (Linux): the session bus's `ActiveChanged(bool)` of `org.freedesktop.ScreenSaver`
//!   (KDE, and the freedesktop name other desktops implement) and `org.gnome.ScreenSaver`. One
//!   match rule on one connection, read by a task that waits on the socket: nothing runs until the
//!   screen locks or unlocks. Windows: not built (`WTSRegisterSessionNotification`; the Windows
//!   batch in `HANDOFF.md`).

use decibell_model::Model;
use gpui::{App, AppContext as _, Subscription};

pub(crate) fn install(cx: &mut App) -> Vec<Subscription> {
    let Some(model) = cx.try_global::<Model>().cloned() else { return Vec::new() };
    let away = model.live.presence.auto_away.clone();
    let sleeping = {
        let away = away.clone();
        cx.on_system_sleep(move |cx| {
            crate::marker(format_args!("session sleep"));
            away.update(cx, |a, cx| a.set_locked(true, cx));
        })
    };
    let waking = {
        let away = away.clone();
        cx.on_system_wake(move |cx| {
            crate::marker(format_args!("session wake"));
            away.update(cx, |a, cx| a.set_locked(false, cx));
        })
    };
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    watch_screen_lock(away, cx);
    vec![sleeping, waking]
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn watch_screen_lock(away: gpui::Entity<decibell_model::live::presence::auto_away::AutoAway>, cx: &mut App) {
    use futures::StreamExt as _;
    let (tx, mut rx) = futures::channel::mpsc::unbounded::<bool>();
    cx.background_spawn(async move {
        if let Err(e) = screen_lock_signals(tx).await {
            log::debug!("[session] no screen-lock signals: {e}");
        }
    })
    .detach();
    cx.spawn(async move |cx| {
        while let Some(locked) = rx.next().await {
            crate::marker(format_args!("session {}", if locked { "locked" } else { "unlocked" }));
            cx.update(|cx| away.update(cx, |a, cx| a.set_locked(locked, cx)));
        }
    })
    .detach();
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
async fn screen_lock_signals(tx: futures::channel::mpsc::UnboundedSender<bool>) -> zbus::Result<()> {
    use futures::StreamExt as _;
    let connection = zbus::Connection::session().await?;
    let rule = |interface: &'static str| {
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(interface)
            .and_then(|b| b.member("ActiveChanged"))
            .map(|b| b.build())
    };
    let freedesktop = zbus::MessageStream::for_match_rule(rule("org.freedesktop.ScreenSaver")?, &connection, None).await?;
    let gnome = zbus::MessageStream::for_match_rule(rule("org.gnome.ScreenSaver")?, &connection, None).await?;
    let mut signals = futures::stream::select(freedesktop, gnome);
    while let Some(message) = signals.next().await {
        let Ok(message) = message else { continue };
        if let Ok(active) = message.body().deserialize::<bool>()
            && tx.unbounded_send(active).is_err()
        {
            break;
        }
    }
    Ok(())
}
