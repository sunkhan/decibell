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
    /// Username cursor: omit/"" for the first page (all online members +
    /// first `limit` offline), else `nextAfter` of the previous page.
    pub after: Option<String>,
    /// Offline members per page (server clamps to [1, 200]; default 100).
    pub limit: Option<i32>,
}

/// Fetch one page of the roster. Live changes arrive as member_upsert /
/// member_remove; this is only for the initial load and scroll paging.
#[napi]
pub async fn list_members(args: ListMembersArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::MemberListReq,
        packet::Payload::MemberListReq(MemberListRequest {
            after: args.after.unwrap_or_default(),
            limit: args.limit.unwrap_or(0),
        }),
    )
    .await
}

#[napi(object)]
pub struct UpdateServerArgs {
    pub server_id: String,
    pub name: String,
    pub description: String,
}

/// MANAGE_SERVER: rename / re-describe the server. Result arrives as
/// server_update_responded; everyone gets server_meta_updated.
#[napi]
pub async fn update_server(args: UpdateServerArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::ServerUpdateReq,
        packet::Payload::ServerUpdateReq(ServerUpdateRequest {
            name: args.name,
            description: args.description,
        }),
    )
    .await
}

#[napi(object)]
pub struct ListAuditLogArgs {
    pub server_id: String,
    /// 0 = newest.
    pub before_id: Option<i64>,
    pub limit: Option<i32>,
}

#[napi]
pub async fn list_audit_log(args: ListAuditLogArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::AuditLogReq,
        packet::Payload::AuditLogReq(AuditLogRequest {
            before_id: args.before_id.unwrap_or(0),
            limit: args.limit.unwrap_or(50),
        }),
    )
    .await
}

#[napi(object)]
pub struct TimeoutMemberArgs {
    pub server_id: String,
    pub username: String,
    /// Unix seconds; 0 clears an active timeout.
    pub until: i64,
    pub reason: Option<String>,
}

/// MODERATE_MEMBERS + hierarchy. Response via mod_action_responded
/// action="timeout"; the member_upsert delta carries timedOutUntil.
#[napi]
pub async fn timeout_member(args: TimeoutMemberArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::TimeoutMemberReq,
        packet::Payload::TimeoutMemberReq(TimeoutMemberRequest {
            username: args.username,
            until: args.until,
            reason: args.reason.unwrap_or_default(),
        }),
    )
    .await
}

#[napi(object)]
pub struct VoiceModArgs {
    pub server_id: String,
    pub username: String,
    /// "server_mute" | "server_unmute" | "server_deafen" | "server_undeafen" | "move" | "disconnect"
    pub action: String,
    /// "move" only.
    pub channel_id: Option<String>,
}

#[napi]
pub async fn voice_mod(args: VoiceModArgs) -> napi::Result<()> {
    let action = match args.action.as_str() {
        "server_mute" => voice_mod_request::Action::ServerMute,
        "server_unmute" => voice_mod_request::Action::ServerUnmute,
        "server_deafen" => voice_mod_request::Action::ServerDeafen,
        "server_undeafen" => voice_mod_request::Action::ServerUndeafen,
        "move" => voice_mod_request::Action::Move,
        "disconnect" => voice_mod_request::Action::Disconnect,
        other => {
            return Err(napi::Error::from_reason(format!(
                "Unknown voice_mod action '{}'",
                other
            )))
        }
    };
    send_for_server(
        &args.server_id,
        packet::Type::VoiceModReq,
        packet::Payload::VoiceModReq(VoiceModRequest {
            username: args.username,
            action: action as i32,
            channel_id: args.channel_id.unwrap_or_default(),
        }),
    )
    .await
}

#[napi(object)]
pub struct TransferOwnershipArgs {
    pub server_id: String,
    pub new_owner: String,
}

/// Owner only. Response via mod_action_responded action="transfer";
/// everyone gets server_meta_updated + member_upsert for both users.
#[napi]
pub async fn transfer_ownership(args: TransferOwnershipArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::TransferOwnershipReq,
        packet::Payload::TransferOwnershipReq(TransferOwnershipRequest {
            new_owner: args.new_owner,
        }),
    )
    .await
}

#[napi(object)]
pub struct ListBansArgs {
    pub server_id: String,
}

/// Fetch the ban list (BAN_MEMBERS). Also pushed by the server on change.
#[napi]
pub async fn list_bans(args: ListBansArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::BanListReq,
        packet::Payload::BanListReq(BanListRequest {}),
    )
    .await
}

#[napi(object)]
pub struct KickMemberArgs {
    pub server_id: String,
    pub username: String,
    /// Optional — the UI doesn't collect a reason yet. A required
    /// String here made napi reject every kick with "Missing field
    /// 'reason'".
    pub reason: Option<String>,
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
        packet::Payload::KickMemberReq(KickMemberRequest {
            username,
            reason: reason.unwrap_or_default(),
        }),
    )
    .await
}

#[napi(object)]
pub struct BanMemberArgs {
    pub server_id: String,
    pub username: String,
    /// Optional — see KickMemberArgs.
    pub reason: Option<String>,
    /// Unix seconds when the ban lifts; omit / 0 = permanent.
    pub expires_at: Option<i64>,
    /// Also delete the member's messages from the last N seconds (≤ 7 days).
    pub delete_message_seconds: Option<i32>,
}

#[napi]
pub async fn ban_member(args: BanMemberArgs) -> napi::Result<()> {
    let BanMemberArgs {
        server_id,
        username,
        reason,
        expires_at,
        delete_message_seconds,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::BanMemberReq,
        packet::Payload::BanMemberReq(BanMemberRequest {
            username,
            reason: reason.unwrap_or_default(),
            expires_at: expires_at.unwrap_or(0),
            delete_message_seconds: delete_message_seconds.unwrap_or(0),
        }),
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

#[napi(object)]
pub struct CreateInviteArgs {
    pub server_id: String,
    /// Unix epoch seconds. 0 means "never expires".
    pub expires_at: i64,
    /// 0 means "unlimited uses".
    pub max_uses: i32,
}

#[napi]
pub async fn create_invite(args: CreateInviteArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::InviteCreateReq,
        packet::Payload::InviteCreateReq(InviteCreateRequest {
            expires_at: args.expires_at,
            max_uses: args.max_uses,
        }),
    )
    .await
}

#[napi(object)]
pub struct ListInvitesArgs {
    pub server_id: String,
}

#[napi]
pub async fn list_invites(args: ListInvitesArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::InviteListReq,
        packet::Payload::InviteListReq(InviteListRequest {}),
    )
    .await
}

#[napi(object)]
pub struct RevokeInviteArgs {
    pub server_id: String,
    pub code: String,
}

#[napi]
pub async fn revoke_invite(args: RevokeInviteArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::InviteRevokeReq,
        packet::Payload::InviteRevokeReq(InviteRevokeRequest { code: args.code }),
    )
    .await
}

// ── Roles + permissions ──────────────────────────────────────────
//
// Results land as `role_action_responded`; the authoritative state
// follows as pushed `role_list_received` / `member_list_received`
// broadcasts. Permission bitfields ride as i64 — every defined bit is
// far below 2^53 so JS numbers round-trip losslessly.

#[napi(object)]
pub struct ListRolesArgs {
    pub server_id: String,
}

#[napi]
pub async fn list_roles(args: ListRolesArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::RoleListReq,
        packet::Payload::RoleListReq(RoleListRequest {}),
    )
    .await
}

#[napi(object)]
pub struct CreateRoleArgs {
    pub server_id: String,
    pub name: String,
    /// 0xRRGGBB; 0 = default color.
    pub color: u32,
    pub permissions: i64,
}

#[napi]
pub async fn create_role(args: CreateRoleArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::RoleCreateReq,
        packet::Payload::RoleCreateReq(RoleCreateRequest {
            name: args.name,
            color: args.color,
            permissions: args.permissions.max(0) as u64,
        }),
    )
    .await
}

#[napi(object)]
pub struct UpdateRoleArgs {
    pub server_id: String,
    pub role_id: i64,
    pub name: String,
    pub color: u32,
    pub permissions: i64,
    pub position: i32,
}

#[napi]
pub async fn update_role(args: UpdateRoleArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::RoleUpdateReq,
        packet::Payload::RoleUpdateReq(RoleUpdateRequest {
            role_id: args.role_id,
            name: args.name,
            color: args.color,
            permissions: args.permissions.max(0) as u64,
            position: args.position,
        }),
    )
    .await
}

#[napi(object)]
pub struct DeleteRoleArgs {
    pub server_id: String,
    pub role_id: i64,
}

#[napi]
pub async fn delete_role(args: DeleteRoleArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::RoleDeleteReq,
        packet::Payload::RoleDeleteReq(RoleDeleteRequest {
            role_id: args.role_id,
        }),
    )
    .await
}

#[napi(object)]
pub struct SetMemberRolesArgs {
    pub server_id: String,
    pub username: String,
    /// The member's full desired role set (default role implicit).
    pub role_ids: Vec<i64>,
}

#[napi]
pub async fn set_member_roles(args: SetMemberRolesArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::MemberRolesUpdateReq,
        packet::Payload::MemberRolesUpdateReq(MemberRolesUpdateRequest {
            username: args.username,
            role_ids: args.role_ids,
        }),
    )
    .await
}

#[napi(object)]
pub struct UnbanMemberArgs {
    pub server_id: String,
    pub username: String,
}

#[napi]
pub async fn unban_member(args: UnbanMemberArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::UnbanMemberReq,
        packet::Payload::UnbanMemberReq(UnbanMemberRequest {
            username: args.username,
        }),
    )
    .await
}

#[napi(object)]
pub struct SetNicknameArgs {
    pub server_id: String,
    /// Target member — may be the local user (self-changes are always
    /// allowed; others need MANAGE_NICKNAMES + a strictly higher role).
    pub username: String,
    /// Empty clears the nickname.
    pub nickname: String,
}

#[napi]
pub async fn set_nickname(args: SetNicknameArgs) -> napi::Result<()> {
    let SetNicknameArgs {
        server_id,
        username,
        nickname,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::SetNicknameReq,
        packet::Payload::SetNicknameReq(SetNicknameRequest { username, nickname }),
    )
    .await
}
