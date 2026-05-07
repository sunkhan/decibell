//! Community-server admin operations: member list, kick, ban, leave.
//! Invite admin (create / list / revoke) ports with the invites PR.
//!
//! Each of these dispatches a packet to the matching community
//! connection and returns immediately. The result lands later as a
//! `member_list_received` / `mod_action_responded` /
//! `membership_revoked` bus event — see net/community.rs::route_packets.

use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state;

async fn send_for_server(
    server_id: &str,
    pkt_type: packet::Type,
    payload: packet::Payload,
) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let client = s.communities.get(server_id).ok_or_else(|| {
            napi::Error::from_reason(format!("Not connected to community {}", server_id))
        })?;
        let tx = client
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Community connection lost"))?;
        let pkt = build_packet(pkt_type, payload, Some(&client.jwt));
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct ListMembersArgs {
    pub server_id: String,
}

#[napi]
pub async fn list_members(args: ListMembersArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::MemberListReq,
        packet::Payload::MemberListReq(MemberListRequest {}),
    )
    .await
}

#[napi(object)]
pub struct KickMemberArgs {
    pub server_id: String,
    pub username: String,
    pub reason: String,
}

#[napi]
pub async fn kick_member(args: KickMemberArgs) -> napi::Result<()> {
    let KickMemberArgs {
        server_id,
        username,
        reason,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::KickMemberReq,
        packet::Payload::KickMemberReq(KickMemberRequest { username, reason }),
    )
    .await
}

#[napi(object)]
pub struct BanMemberArgs {
    pub server_id: String,
    pub username: String,
    pub reason: String,
}

#[napi]
pub async fn ban_member(args: BanMemberArgs) -> napi::Result<()> {
    let BanMemberArgs {
        server_id,
        username,
        reason,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::BanMemberReq,
        packet::Payload::BanMemberReq(BanMemberRequest { username, reason }),
    )
    .await
}

#[napi(object)]
pub struct LeaveServerArgs {
    pub server_id: String,
}

#[napi]
pub async fn leave_server(args: LeaveServerArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::LeaveServerReq,
        packet::Payload::LeaveServerReq(LeaveServerRequest {}),
    )
    .await
}
