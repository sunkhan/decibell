//! Statuses, status messages, the watch set's requests, auto-away's reports, avatars
//! (`features/presence/StatusMenu.tsx`' `chooseStatus`, `CustomStatusModal.tsx`'
//! `saveStatusMessage`, `presenceInterest.ts`, `useAutoAway.ts`, `avatarStore.ts`'
//! `fetch_avatar`).

use std::time::Duration;

use decibell_core::api;
use futures::future::LocalBoxFuture;
use gpui::App;

use super::voice::{core, fire};
use crate::Model;
use crate::error::UiError;
use crate::ids::Uid;
use crate::live::presence::statuses::{STATUS_TEXT_MAX, normalize_status_text};
use crate::live::presence::{IdleReading, IdleSource, OwnStatus, StatusMessage};

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

/// `PresenceInterest`'s requests: the whole watch set (the core keeps it and re-sends it
/// after every sign-in).
pub(crate) fn subscribe(uids: Vec<Uid>) -> LocalBoxFuture<'static, Result<(), UiError>> {
    Box::pin(async move { core(api::presence::presence_subscribe(api::presence::PresenceSubscribeArgs { uids })).await })
}

/// `Avatars`' fetches.
pub(crate) fn fetch_avatar(uid: Uid) -> LocalBoxFuture<'static, Result<(String, Vec<u8>), UiError>> {
    Box::pin(async move {
        let r = core(api::auth::fetch_avatar(api::auth::FetchAvatarArgs { uid })).await?;
        Ok((r.version, r.data))
    })
}

/// Auto-away's report: our idle flag to the own-presence store and to central.
pub(crate) fn report_idle(idle: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.live.presence.own.update(cx, |o, cx| o.set_idle(idle, cx));
    fire("set_idle", api::presence::set_idle(api::presence::SetIdleArgs { idle }), cx);
}

/// The status menu (optimistic: the menu shows the pick at once; central applies it,
/// saves it for the next sign-in, and echoes it to our other devices).
pub fn choose_status(status: OwnStatus, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.live.presence.own.update(cx, |o, cx| o.set_status(status, cx));
    fire("set_status", api::presence::set_status(api::presence::SetStatusArgs { status: status.as_wire().into() }), cx);
}

/// Save our custom status (optimistic, normalised the way central will). `clear_after`:
/// seconds, 0 = never ("Today" is the modal's: seconds to local midnight, at least 60).
pub fn save_status_message(message: Option<StatusMessage>, clear_after: u32, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let text = message.as_ref().map(|m| normalize_status_text(&m.text, STATUS_TEXT_MAX)).unwrap_or_default();
    let emoji = message.as_ref().map(|m| m.emoji.to_string()).unwrap_or_default();
    let next = StatusMessage::of(&text, &emoji);
    let after = if next.is_some() { clear_after } else { 0 };
    let clears_at = (after > 0).then(|| cx.background_executor().now() + Duration::from_secs(u64::from(after)));
    model.live.presence.own.update(cx, |o, cx| o.set_message(next, clears_at, cx));
    fire(
        "set_status_message",
        api::presence::set_status_message(api::presence::SetStatusMessageArgs { text, emoji, clear_after: after }),
        cx,
    );
}

/// A view shows these users: watch them while it does. Pair with [`release`].
pub fn retain(uids: impl IntoIterator<Item = Uid>, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.live.presence.interest.update(cx, |i, cx| i.retain(uids, cx));
}

pub fn release(uids: impl IntoIterator<Item = Uid>, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.live.presence.interest.update(cx, |i, cx| i.release(uids, cx));
}

/// The idle source the core offers: Wayland's `ext-idle-notify` and GNOME's IdleMonitor,
/// the screensaver's lock flag; "unsupported" on X11, Windows and macOS, where the app
/// supplies its own source. The core's answer carries no idle duration and nothing is
/// pushed, so `AutoAway` re-reads one threshold later (see its module doc).
pub struct CoreIdleSource;

impl IdleSource for CoreIdleSource {
    fn read(&self, threshold: Duration) -> LocalBoxFuture<'static, IdleReading> {
        let threshold_seconds = threshold.as_secs().min(u64::from(u32::MAX)) as u32;
        Box::pin(async move {
            match core(api::presence::system_idle_state(api::presence::SystemIdleArgs { threshold_seconds })).await.as_deref() {
                Ok("idle") => IdleReading::Idle,
                Ok("active") => IdleReading::Active { idle_for: None },
                Ok("locked") => IdleReading::Locked,
                _ => IdleReading::Unsupported,
            }
        })
    }

    fn pushes_resume(&self) -> bool {
        false
    }
}
