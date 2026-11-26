//! Hotkeys, the device list, and the M3 hooks of sign-in / sign-out
//! (`features/hotkeys/hotkeyRuntime.ts`, `useAuthEvents.ts`' `devices_changed`,
//! `MainLayout.tsx`'s mount effects, the sign-out half of the stores).

use std::rc::Rc;
use std::time::Duration;

use decibell_core::events::{DevicesChangedPayload, HotkeyActionPayload};
use gpui::{App, SharedString};

use crate::Model;
use crate::live::devices::device_title;
use crate::stores::overlays::modal;
use crate::stores::toasts::{Severity, ToastAction};

pub fn hotkey_action(_model: &Model, p: &HotkeyActionPayload, cx: &mut App) {
    crate::actions::hotkeys::run_action(p.action, p.pressed, cx);
}

pub fn hotkeys_status(model: &Model, p: &decibell_core::hotkeys::Status, cx: &mut App) {
    let settings = model.settings.read(cx).get().clone();
    model.live.hotkeys.update(cx, |h, cx| h.set_status(p.clone(), &settings, cx));
}

/// The account's device list changed: an open Devices tab reloads; a typed sign-in on a
/// new device is announced for 20 s with a way to review the devices.
pub fn devices_changed(model: &Model, p: &DevicesChangedPayload, cx: &mut App) {
    let watching = model.live.devices.update(cx, |d, cx| {
        d.bump(cx);
        d.watching > 0
    });
    if watching {
        crate::actions::devices::load(cx);
    }
    let Some(d) = &p.new_device else { return };
    let title = device_title(&d.name, &d.platform);
    let place = [title.to_string(), d.address.clone()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
    let body: SharedString = format!("{place}. If this wasn't you, sign that device out and change your password.").into();
    let m = model.clone();
    let action = ToastAction {
        label: "Review devices".into(),
        run: Rc::new(move |cx: &mut App| {
            m.nav.update(cx, |nav, cx| nav.set_settings_tab("devices", cx));
            m.overlays.update(cx, |o, cx| o.open_modal(modal::SETTINGS, cx));
        }),
    };
    model.toasts.update(cx, |t, cx| {
        t.push(Severity::Warning, "New sign-in to your account", Some(body), Some(Duration::from_secs(20)), Some(action), cx)
    });
}

/// `login_succeeded`: the friend list (MainLayout asked on mount; after a dropped
/// connection `useAuthEvents` asked again), central's call support, and on the first
/// sign-in of the session the pulls a mounting renderer did (E2EE status, the hotkey
/// listener's status). The presence subscriptions are re-sent by the core itself.
pub fn signed_in(model: &Model, again: bool, cx: &mut App) {
    crate::actions::friends::request_friend_list(cx);
    crate::actions::calls::refresh_config(cx);
    if !again {
        crate::actions::e2ee::pull_status(cx);
        crate::actions::hotkeys::pull_status(cx);
    }
    let _ = model;
}

/// `logged_out`: the core dropped the voice, stream and call engines; the UI's half goes
/// with them, and nothing about people carries over to the next account.
pub fn logged_out(model: &Model, cx: &mut App) {
    let live = model.live.clone();
    live.cues.update(cx, |c, cx| c.stop_ring(cx));
    live.calls.update(cx, |c, cx| c.reset(None, cx));
    crate::actions::voice::disconnect(model, cx);
    // Every community goes with the sign-out (`Communities::reset_for_logout`): so do
    // their voice and stream rosters (the renderer kept them until the next sign-in's
    // snapshots replaced them).
    live.voice.rosters.update(cx, |r, cx| r.reset(cx));
    live.voice.streams.update(cx, |s, cx| s.reset(cx));
    live.voice.session.update(cx, |s, cx| s.reset(cx));
    live.presence.statuses.update(cx, |s, cx| s.mark_disconnected(cx));
    live.presence.messages.update(cx, |m, cx| m.clear(cx));
    live.presence.own.update(cx, |o, cx| o.reset(cx));
    live.presence.auto_away.update(cx, |a, cx| a.reset(cx));
    live.presence.avatars.update(cx, |a, cx| a.clear(cx));
    live.friends.update(cx, |f, cx| f.reset(cx));
    live.devices.update(cx, |d, cx| d.reset(cx));
}
