//! Friends list + per-friend actions (add, remove, block, accept,
//! reject). All routed through the central server. Responses arrive
//! asynchronously as `friend_list_received` / `friend_action_responded`
//! bus events — see net/central.rs::route_packets.

use crate::net::connection::build_packet;
use crate::net::proto::{packet, FriendActionReq, FriendListReq, FriendsInCommunityRequest, FriendsInCommunityResponse};
use crate::state;

#[napi]
pub async fn request_friend_list() -> napi::Result<()> {
    let state_arc = state::shared();

    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let token = s.token.clone();
        let central = s
            .central
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not connected to central server"))?;
        let tx = central
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Central connection lost"))?;
        let pkt = build_packet(
            packet::Type::FriendListReq,
            packet::Payload::FriendListReq(FriendListReq {}),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct SendFriendActionArgs {
    /// Maps to the protobuf FriendActionType enum:
    /// 0 = ADD, 1 = REMOVE, 2 = BLOCK, 3 = ACCEPT, 4 = REJECT.
    pub action: i32,
    pub target_username: String,
    /// The target's uid (preferred over the name, which stays for names
    /// a person types).
    pub target_uid: Option<i64>,
}

#[napi]
pub async fn send_friend_action(args: SendFriendActionArgs) -> napi::Result<()> {
    let state_arc = state::shared();

    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let token = s.token.clone();
        let central = s
            .central
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not connected to central server"))?;
        let tx = central
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Central connection lost"))?;
        let pkt = build_packet(
            packet::Type::FriendActionReq,
            packet::Payload::FriendActionReq(FriendActionReq {
                action: args.action,
                target_username: args.target_username,
                target_uid: args.target_uid.unwrap_or(0),
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct FriendsInCommunityArgs {
    pub server_id: String,
}

#[napi(object)]
pub struct FriendsInCommunity {
    /// False when central couldn't say: not a community it lists, not one
    /// we're a member of, or its database failed. The caller then judges
    /// on what it has.
    pub known: bool,
    pub member_uids: Vec<i64>,
}

/// The welcome banner's "Invite friends": which of our accepted friends are
/// already members of this community. Central answers from its membership
/// table, so the friend list never reaches the community (owner rule).
/// Communities central doesn't list (a raw host:port join) answer `known:
/// false`. Times out after 5 seconds.
#[napi]
pub async fn friends_in_community(args: FriendsInCommunityArgs) -> napi::Result<FriendsInCommunity> {
    let unknown = FriendsInCommunity { known: false, member_uids: Vec::new() };
    let Ok(server_id) = args.server_id.parse::<i64>() else { return Ok(unknown) };
    let state_arc = state::shared();
    let (write_tx, data, rx) = {
        let mut s = state_arc.lock().await;
        let token = s.token.clone();
        let central = s
            .central
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not connected to central server"))?;
        let tx = central
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Central connection lost"))?;
        let pkt = build_packet(
            packet::Type::FriendsInCommunityReq,
            packet::Payload::FriendsInCommunityReq(FriendsInCommunityRequest { server_id }),
            token.as_deref(),
        );
        let (otx, orx) = tokio::sync::oneshot::channel::<FriendsInCommunityResponse>();
        s.pending_friends_in_community.insert(server_id, otx);
        (tx, pkt, orx)
    };
    let timeout = std::time::Duration::from_secs(5);
    if tokio::time::timeout(timeout, write_tx.send(data)).await.map_or(true, |r| r.is_err()) {
        state_arc.lock().await.pending_friends_in_community.remove(&server_id);
        return Err(napi::Error::from_reason("Couldn't reach central"));
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(res)) if res.success => Ok(FriendsInCommunity { known: true, member_uids: res.member_uids }),
        Ok(Ok(_)) => Ok(unknown),
        Ok(Err(_)) => Err(napi::Error::from_reason("Central connection closed before the answer")),
        Err(_) => {
            state_arc.lock().await.pending_friends_in_community.remove(&server_id);
            Err(napi::Error::from_reason("Timed out waiting for central"))
        }
    }
}

