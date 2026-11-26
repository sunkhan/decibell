//! `#[napi]` wrappers over `decibell_core::api::community`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::community::{
    BanMemberArgs, CreateInviteArgs, CreateRoleArgs, DeleteRoleArgs, KickMemberArgs,
    LeaveServerArgs, ListAuditLogArgs, ListBansArgs, ListInvitesArgs, ListMembersArgs,
    ListRolesArgs, RevokeInviteArgs, SearchMembersArgs, SetMemberRolesArgs, SetNicknameArgs,
    SetStorageMinFreeArgs, StorageInfoArgs, TimeoutMemberArgs, TransferOwnershipArgs,
    UnbanMemberArgs, UpdateRoleArgs, UpdateServerArgs, VoiceModArgs,
};

/// Fetch one page of the roster, or look members up by uid. Live changes
/// arrive as member_upsert / member_remove; this is only for the initial
/// load, scroll paging and the members a view shows before paging reaches
/// them.
#[napi]
pub async fn list_members(args: ListMembersArgs) -> napi::Result<()> {
    api::community::list_members(args)
        .await
        .map_err(napi::Error::from)
}

/// Server-settings member search across the whole membership (online or
/// not). The page arrives as member_search_received; it never touches the
/// roster.
#[napi]
pub async fn search_members(args: SearchMembersArgs) -> napi::Result<()> {
    api::community::search_members(args)
        .await
        .map_err(napi::Error::from)
}

/// MANAGE_SERVER: rename / re-describe the server and/or toggle public
/// listing. Result arrives as server_update_responded; everyone gets
/// server_meta_updated.
#[napi]
pub async fn update_server(args: UpdateServerArgs) -> napi::Result<()> {
    api::community::update_server(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn list_audit_log(args: ListAuditLogArgs) -> napi::Result<()> {
    api::community::list_audit_log(args)
        .await
        .map_err(napi::Error::from)
}

/// MANAGE_SERVER: fetch host disk state + this server's footprint. Result
/// arrives as storage_info_received.
#[napi]
pub async fn get_storage_info(args: StorageInfoArgs) -> napi::Result<()> {
    api::community::get_storage_info(args)
        .await
        .map_err(napi::Error::from)
}

/// MANAGE_SERVER: set the upload headroom. Server replies with a fresh
/// storage_info_received reflecting the applied (clamped) value.
#[napi]
pub async fn set_storage_min_free(args: SetStorageMinFreeArgs) -> napi::Result<()> {
    api::community::set_storage_min_free(args)
        .await
        .map_err(napi::Error::from)
}

/// MODERATE_MEMBERS + hierarchy. Response via mod_action_responded
/// action="timeout"; the member_upsert delta carries timedOutUntil.
#[napi]
pub async fn timeout_member(args: TimeoutMemberArgs) -> napi::Result<()> {
    api::community::timeout_member(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn voice_mod(args: VoiceModArgs) -> napi::Result<()> {
    api::community::voice_mod(args)
        .await
        .map_err(napi::Error::from)
}

/// Owner only. Response via mod_action_responded action="transfer";
/// everyone gets server_meta_updated + member_upsert for both users.
#[napi]
pub async fn transfer_ownership(args: TransferOwnershipArgs) -> napi::Result<()> {
    api::community::transfer_ownership(args)
        .await
        .map_err(napi::Error::from)
}

/// Fetch the ban list (BAN_MEMBERS). Also pushed by the server on change.
#[napi]
pub async fn list_bans(args: ListBansArgs) -> napi::Result<()> {
    api::community::list_bans(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn kick_member(args: KickMemberArgs) -> napi::Result<()> {
    api::community::kick_member(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn ban_member(args: BanMemberArgs) -> napi::Result<()> {
    api::community::ban_member(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn leave_server(args: LeaveServerArgs) -> napi::Result<()> {
    api::community::leave_server(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn create_invite(args: CreateInviteArgs) -> napi::Result<()> {
    api::community::create_invite(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn list_invites(args: ListInvitesArgs) -> napi::Result<()> {
    api::community::list_invites(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn revoke_invite(args: RevokeInviteArgs) -> napi::Result<()> {
    api::community::revoke_invite(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn list_roles(args: ListRolesArgs) -> napi::Result<()> {
    api::community::list_roles(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn create_role(args: CreateRoleArgs) -> napi::Result<()> {
    api::community::create_role(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn update_role(args: UpdateRoleArgs) -> napi::Result<()> {
    api::community::update_role(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn delete_role(args: DeleteRoleArgs) -> napi::Result<()> {
    api::community::delete_role(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_member_roles(args: SetMemberRolesArgs) -> napi::Result<()> {
    api::community::set_member_roles(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn unban_member(args: UnbanMemberArgs) -> napi::Result<()> {
    api::community::unban_member(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_nickname(args: SetNicknameArgs) -> napi::Result<()> {
    api::community::set_nickname(args)
        .await
        .map_err(napi::Error::from)
}
