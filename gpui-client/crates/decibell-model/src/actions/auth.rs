//! Signing in and out (`features/auth/LoginPage.tsx`, `features/settings/loadSettings.ts`,
//! `AccountTab.tsx`). The answers are events: `events/auth.rs`.

use std::rc::Rc;

use decibell_core::api::{self, auth::LoginArgs, auth::RegisterArgs, auth::RenameAccountArgs};
use decibell_core::config::AppSettings;
use gpui::{App, AsyncApp, SharedString};

use crate::Model;
use crate::call::{call, offer_retrust};
use crate::error::UiError;
use crate::stores::auth::RegisterResult;
use crate::stores::overlays::Retry;

/// What the renderer does before its first paint (`loadSettings`): the config into the
/// settings store, then the automatic sign-in when a sign-in is saved (the password
/// never leaves the core). A failure (server gone, no internet) just lands on the
/// sign-in page with no error; a password central refuses is forgotten by the core, so
/// it isn't retried every launch.
pub(crate) async fn boot(model: &Model, core_running: bool, cx: &mut AsyncApp) {
    let mut saved_username: Option<SharedString> = None;
    if core_running {
        match api::settings::load_config().await {
            Ok(mut config) => {
                saved_username = config["saved_username"].as_str().map(|s| s.to_string().into());
                match serde_json::from_value::<AppSettings>(config["settings"].take()) {
                    Ok(settings) => model.settings.update(cx, |s, cx| s.apply_loaded(settings, cx)),
                    Err(e) => log::warn!("[loadSettings] config doesn't parse; using defaults: {e}"),
                }
            }
            Err(e) => log::warn!("[loadSettings] load_config failed; using defaults: {e}"),
        }
    }
    if saved_username.is_some() {
        model.auth.update(cx, |a, cx| a.set_logging_in(true, cx));
        match api::auth::auto_login().await {
            Ok(true) => {}
            Ok(false) => model.auth.update(cx, |a, cx| a.set_logging_in(false, cx)),
            Err(_) => model.auth.update(cx, |a, cx| a.set_login_error(None, cx)),
        }
    }
    model.auth.update(cx, |a, cx| a.set_booted(saved_username, cx));
}

/// The sign-in page's submit (`handleLogin`). Success is the `login_succeeded` event.
pub fn sign_in(username: String, password: String, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    model.auth.update(cx, |a, cx| a.set_logging_in(true, cx));
    let retry = {
        let (username, password) = (username.clone(), password.clone());
        Rc::new(move |cx: &mut App| sign_in(username.clone(), password.clone(), cx)) as Retry
    };
    call(cx, api::auth::login(LoginArgs { username, password }), move |result, cx| {
        let Err(e) = result else { return };
        if offer_retrust(&e, Some(retry), cx) {
            model.auth.update(cx, |a, cx| a.set_logging_in(false, cx));
        } else {
            model.auth.update(cx, |a, cx| a.set_login_error(Some(e.js_string()), cx));
        }
    });
}

/// The sign-up form's submit (`handleRegister`). The answer is `register_responded`.
pub fn register(username: String, email: String, password: String, cx: &mut App) {
    let Some(model) = cx.try_global::<Model>().cloned() else { return };
    model.auth.update(cx, |a, cx| a.set_registering(true, cx));
    call(cx, api::auth::register(RegisterArgs { username, email, password }), move |result, cx| {
        if let Err(e) = result {
            let failed = RegisterResult { success: false, message: e.js_string() };
            model.auth.update(cx, |a, cx| a.set_register_result(Some(failed), cx));
        }
    });
}

/// Log out (`AccountTab.handleLogout`): the core forgets this device at central, drops
/// every connection and the saved sign-in, and emits `logged_out`, which clears the
/// stores. A failure is only logged, as in the renderer. (Ending a call first is the
/// call feature's to add in front of this.)
pub fn sign_out(cx: &mut App) {
    call(cx, api::auth::logout(), |result, _| {
        if let Err(e) = result {
            log::error!("Logout failed: {e}");
        }
    });
}

/// Settings → Account: change the username (password-confirmed). `done` gets the new
/// name or central's refusal; the stores follow the `user_renamed` event.
pub fn rename_account(
    new_username: String,
    password: String,
    cx: &mut App,
    done: impl FnOnce(Result<SharedString, UiError>, &mut App) + 'static,
) {
    call(
        cx,
        api::auth::rename_account(RenameAccountArgs { new_username, password }),
        move |result, cx| done(result.map(|r| r.username.into()), cx),
    );
}
