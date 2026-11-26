//! Sign-in, sign-out, registration, renames (`features/auth/useAuthEvents.ts`; the
//! `server_list_received` listener of that hook is in `servers.rs`, with the other
//! community lists).

use decibell_core::events::{
    LoggedOutPayload, LoginFailedPayload, LoginSucceededPayload, RegisterRespondedPayload, UserRenamedPayload,
};
use gpui::{App, SharedString};

use super::todo;
use crate::Model;
use crate::actions;
use crate::stores::auth::RegisterResult;
use crate::stores::navigation::ActiveView;

pub fn login_succeeded(model: &Model, p: &LoginSucceededPayload, cx: &mut App) {
    let uid = p.uid;
    // Central signed the same account in again on its own: a resume, or the password
    // after a refused resume, following a dropped connection.
    let again = {
        let auth = model.auth.read(cx);
        auth.is_authenticated && uid > 0 && auth.uid == uid
    };
    model
        .auth
        .update(cx, |a, cx| a.login(p.username.clone().into(), p.renames, uid, p.devices, cx));
    // Others' pictures come with their presence; nothing sends our own.
    todo::m3::seed_own_avatar(model, uid, cx);
    // The public directory, so the server bar and the browse view have data.
    actions::servers::request_server_list(cx);
    // DM previews; and after a reconnect what happened while we were away (review B4).
    todo::m2::signed_in(model, again, cx);
    todo::m3::signed_in(model, again, cx);
}

pub fn login_failed(model: &Model, p: &LoginFailedPayload, cx: &mut App) {
    let message: SharedString = p.message.clone().into();
    let signed_in = model.auth.read(cx).is_authenticated;
    model.auth.update(cx, |a, cx| a.set_login_error(Some(message.clone()), cx));
    // Signed in already: central refused to sign this device back in after a dropped
    // connection (the account is on as many devices as it may be). The core keeps
    // trying; say why chat is offline.
    if signed_in {
        model.toasts.update(cx, |t, cx| t.warning("Can't reconnect", Some(message), cx));
    }
}

pub fn register_responded(model: &Model, p: &RegisterRespondedPayload, cx: &mut App) {
    let result = RegisterResult { success: p.success, message: p.message.clone().into() };
    model.auth.update(cx, |a, cx| a.set_register_result(Some(result), cx));
}

/// Our own rename: the users directory already holds the new name (`users.rs`).
pub fn user_renamed(model: &Model, p: &UserRenamedPayload, cx: &mut App) {
    if p.is_self {
        model.auth.update(cx, |a, cx| a.set_username(p.username.clone().into(), cx));
    }
}

/// This device is signed out: our own Log out (`reason` none), another device signing it
/// out, or a password change. Nothing carries over to the next account.
pub fn logged_out(model: &Model, p: &LoggedOutPayload, cx: &mut App) {
    // The core dropped the voice, stream and call engines; the UI's half goes with them.
    todo::m3::logged_out(model, cx);
    model.overlays.update(cx, |o, cx| o.close_all_for_logout(cx));
    // The image viewer, attachment tokens and decryption keys, DMs, drafts.
    todo::m2::logged_out(model, cx);
    let notice = match p.reason.as_deref() {
        Some("password_changed") => {
            Some("Your password was changed on another device. Sign in with the new one.")
        }
        Some("signed_out") => Some("You were signed out on this device. Sign in again to keep using it here."),
        _ => None,
    };
    model.auth.update(cx, |a, cx| {
        a.logout(cx);
        a.set_login_notice(notice.map(SharedString::new_static), cx);
    });
    model.servers.update(cx, |s, cx| s.reset_for_logout(cx));
    model.connections.update(cx, |c, cx| c.reset_for_logout(cx));
    model.communities.update(cx, |c, cx| c.reset_for_logout(cx));
    model.nav.update(cx, |nav, cx| {
        nav.set_active_server(None, cx);
        nav.set_active_channel(None, cx);
        nav.set_active_view(ActiveView::Home, cx);
    });
}
