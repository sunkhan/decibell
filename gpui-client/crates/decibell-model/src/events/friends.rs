//! The friend list (`features/friends/useFriendsEvents.ts`).

use decibell_core::events::{FriendActionRespondedPayload, FriendListReceivedPayload};
use gpui::{App, SharedString};

use crate::Model;
use crate::ids::Uid;
use crate::live::friends::Friend;

/// A whole list (asked for or pushed after a change). Every friend's picture version
/// feeds the avatar cache.
pub fn friend_list_received(model: &Model, p: &FriendListReceivedPayload, cx: &mut App) {
    let list: Vec<Friend> = p.friends.iter().filter_map(Friend::from_payload).collect();
    model.live.friends.update(cx, |f, cx| f.set_list(list, cx));
    let versions: Vec<(Uid, SharedString)> = p.friends.iter().map(|f| (f.uid, f.avatar_version.clone().into())).collect();
    model.live.presence.avatars.update(cx, |a, cx| a.set_versions(versions, cx));
}

/// Central refused (or accepted) a friend action: the refusal shows inline.
pub fn friend_action_responded(model: &Model, p: &FriendActionRespondedPayload, cx: &mut App) {
    let error: Option<SharedString> = (!p.success && !p.message.is_empty()).then(|| p.message.clone().into());
    model.live.friends.update(cx, |f, cx| f.set_error(error, cx));
}
