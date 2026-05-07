//! Friends list + per-friend actions (add, remove, block, accept,
//! reject). All routed through the central server. Responses arrive
//! asynchronously as `friend_list_received` / `friend_action_responded`
//! bus events — see net/central.rs::route_packets.

use crate::net::connection::build_packet;
use crate::net::proto::{packet, FriendActionReq, FriendListReq};
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
