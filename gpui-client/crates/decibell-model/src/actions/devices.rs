//! Settings → Devices and the password change (`DevicesTab.tsx`, `ChangePasswordModal.tsx`).

use decibell_core::api;
use gpui::{App, SharedString};

use crate::Model;
use crate::call::call;
use crate::live::devices::{Confirming, Device, PasswordFlow, validate_password_change};

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

/// Load the list (the tab opens; `devices_changed` while it is open).
pub fn load(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    call(cx, api::devices::list_devices(), move |result, cx| {
        let result = result.map(|list| list.into_iter().map(Device::from).collect()).map_err(|e| e.plain());
        model.live.devices.update(cx, |d, cx| d.set_list(result, cx));
    });
}

/// The Devices tab is shown (a `devices_changed` then reloads at once) / hidden.
pub fn watch(on: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.live.devices.update(cx, |d, cx| {
        // The renderer's list is the tab's own state (`useState(null)`): every opening starts
        // from "Loading your devices…", never from what the last opening showed.
        if on && d.watching == 0 && (d.list.is_some() || d.error.is_some()) {
            d.list = None;
            d.error = None;
            cx.notify();
        }
        d.watching = if on { d.watching + 1 } else { d.watching.saturating_sub(1) };
    });
    if on {
        load(cx);
    }
}

/// Sign out one device, or every other one. Toasts the outcome, ends the confirmation,
/// reloads the list.
pub fn sign_out(ids: Vec<i64>, all_others: bool, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.live.devices.update(cx, |d, cx| d.set_busy(true, cx));
    let args = api::devices::SignOutDevicesArgs { ids, all_others: Some(all_others) };
    call(cx, api::devices::sign_out_devices(args), move |result, cx| {
        model.toasts.update(cx, |t, cx| match &result {
            Ok(r) if all_others => {
                let body = if r.signed_out == 1 {
                    "1 other device was signed out.".to_string()
                } else {
                    format!("{} other devices were signed out.", crate::regional::Regional::get().group(r.signed_out as u64))
                };
                t.success("Signed out", Some(body.into()), cx);
            }
            Ok(_) => {
                t.success("Device signed out", None, cx);
            }
            Err(e) => {
                t.error("Couldn't sign out", Some(e.plain()), cx);
            }
        });
        model.live.devices.update(cx, |d, cx| {
            d.set_busy(false, cx);
            d.confirm(None::<Confirming>, cx);
        });
        load(cx);
    });
}

/// The change-password dialog's submit: checked here first, then central confirms the
/// current password and signs every other device out.
pub fn change_password(current: String, new: String, repeat: String, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    if model.live.devices.read(cx).password.as_ref().is_none_or(|p| p.busy) {
        return;
    }
    if let Err(error) = validate_password_change(&current, &new, &repeat) {
        model.live.devices.update(cx, |d, cx| d.set_password(Some(PasswordFlow { busy: false, error: Some(error) }), cx));
        return;
    }
    model.live.devices.update(cx, |d, cx| d.set_password(Some(PasswordFlow { busy: true, error: None }), cx));
    let args = api::devices::ChangePasswordArgs { current_password: current, new_password: new };
    call(cx, api::devices::change_password(args), move |result, cx| match result {
        Ok(r) => {
            let body: Option<SharedString> = match r.signed_out {
                0 => None,
                1 => Some("1 other device was signed out.".into()),
                n => Some(format!("{} other devices were signed out.", crate::regional::Regional::get().group(n as u64)).into()),
            };
            model.toasts.update(cx, |t, cx| t.success("Password changed", body, cx));
            model.live.devices.update(cx, |d, cx| d.set_password(None, cx));
        }
        Err(e) => model.live.devices.update(cx, |d, cx| {
            d.set_password(Some(PasswordFlow { busy: false, error: Some(e.plain()) }), cx)
        }),
    });
}
