//! Friend actions (`features/friends/FriendsPage.tsx`, `FriendsList.tsx`,
//! `FriendActionButton.tsx`, `UserProfileModal.tsx`'s buttons, `friendList.ts`).
//!
//! A refusal from central arrives as `friend_action_responded` (shown inline on the
//! Friends page); a send failure shows inline too, with JS's "Error: " prefix as the
//! renderer's `String(e)` had it. The add form keeps its own error ([`add_friend`]'s
//! callback). Success needs nothing here: central pushes the new list.

use decibell_core::api;
use futures::future::LocalBoxFuture;
use gpui::{App, SharedString};

use super::voice::core;
use crate::Model;
use crate::call::call;
use crate::error::UiError;
use crate::ids::Uid;
use crate::live::friends::FriendAction;

fn model(cx: &App) -> Option<Model> {
    cx.try_global::<Model>().cloned()
}

/// `Friends`' list requests.
pub(crate) fn send_list_request() -> LocalBoxFuture<'static, Result<(), UiError>> {
    Box::pin(async { core(api::friends::request_friend_list()).await })
}

/// Ask for the friend list (again while none arrives).
pub fn request_friend_list(cx: &mut App) {
    let Some(model) = model(cx) else { return };
    model.live.friends.update(cx, |f, cx| f.request(cx));
}

/// Accept, decline, cancel, remove, block, unblock someone already on screen (by uid).
pub fn act(action: FriendAction, uid: Uid, name: SharedString, cx: &mut App) {
    let Some(model) = model(cx) else { return };
    let request = api::friends::send_friend_action(api::friends::SendFriendActionArgs {
        action: action as i32,
        target_username: name.to_string(),
        target_uid: Some(uid),
    });
    call(cx, request, move |result, cx| {
        if let Err(e) = result {
            model.live.friends.update(cx, |f, cx| f.set_error(Some(e.js_string()), cx));
        }
    });
}

/// The add form: a typed name (central resolves it). `done` gets the form's error, if
/// the request could not be sent.
pub fn add_friend(username: String, cx: &mut App, done: impl FnOnce(Result<(), SharedString>, &mut App) + 'static) {
    let target = username.trim().to_string();
    if target.is_empty() {
        return;
    }
    if let Some(model) = model(cx) {
        model.live.friends.update(cx, |f, cx| f.set_error(None, cx));
    }
    let request = api::friends::send_friend_action(api::friends::SendFriendActionArgs {
        action: FriendAction::Add as i32,
        target_username: target,
        target_uid: None,
    });
    call(cx, request, move |result, cx| done(result.map_err(|e| e.js_string()), cx));
}
