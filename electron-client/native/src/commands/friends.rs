//! `#[napi]` wrappers over `decibell_core::api::friends`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::friends::{FriendsInCommunity, FriendsInCommunityArgs, SendFriendActionArgs};

#[napi]
pub async fn request_friend_list() -> napi::Result<()> {
    api::friends::request_friend_list()
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn send_friend_action(args: SendFriendActionArgs) -> napi::Result<()> {
    api::friends::send_friend_action(args)
        .await
        .map_err(napi::Error::from)
}

/// The welcome banner's "Invite friends": which of our accepted friends are
/// already members of this community. Central answers from its membership
/// table, so the friend list never reaches the community (owner rule).
/// Communities central doesn't list (a raw host:port join) answer `known:
/// false`. Times out after 5 seconds.
#[napi]
pub async fn friends_in_community(
    args: FriendsInCommunityArgs,
) -> napi::Result<FriendsInCommunity> {
    api::friends::friends_in_community(args)
        .await
        .map_err(napi::Error::from)
}
