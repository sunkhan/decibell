//! What the settings windows ask the community for: one enum, sent through [`send`]. The model
//! has no actions for these (docs/model.md lists the answers as "answers" delivered to whoever
//! asked), so the calls into `decibell_core::api` live here, in one place.
//!
//! A test or the screens' fixture installs a [`Requester`] with [`install`]: it sees every
//! request first and may answer it itself (the fixture's server, which replies through the
//! model's real router) or let it through (a recorder that checks what was sent).

use std::rc::Rc;

use decibell_core::api;
use decibell_model::call::call;
use decibell_model::{ChannelId, ServerId, UiError};
use gpui::{App, Global};

/// One request, with everything it sends.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    ListBans { server: ServerId },
    ListRoles { server: ServerId },
    SearchMembers { server: ServerId, query: String, role_id: i64, after: String, limit: i32, seq: u32 },
    UpdateServer { server: ServerId, name: String, description: String, public_listing: Option<bool> },
    TransferOwnership { server: ServerId, new_owner: String, new_owner_uid: i64 },
    UpdateServerPicture { server: ServerId, bytes: Rc<Vec<u8>> },
    LeaveServer { server: ServerId },
    KickMember { server: ServerId, username: String, uid: i64, reason: String },
    BanMember { server: ServerId, username: String, uid: i64, reason: String, expires_at: i64, delete_message_seconds: i32 },
    TimeoutMember { server: ServerId, username: String, uid: i64, until: i64, reason: String },
    SetNickname { server: ServerId, username: String, uid: i64, nickname: String },
    SetMemberRoles { server: ServerId, username: String, uid: i64, role_ids: Vec<i64> },
    CreateRole { server: ServerId, name: String, color: u32, permissions: u64, manage_each_other: bool },
    UpdateRole { server: ServerId, role_id: i64, name: String, color: u32, permissions: u64, position: i32, manage_each_other: bool },
    DeleteRole { server: ServerId, role_id: i64 },
    UnbanMember { server: ServerId, username: String, uid: i64 },
    ListAuditLog { server: ServerId, before_id: i64, limit: i32, query: String, actions: Vec<String>, seq: u32 },
    StorageInfo { server: ServerId },
    SetStorageMinFree { server: ServerId, min_free_bytes: i64 },
    RenameChannel { server: ServerId, channel: ChannelId, name: String },
    /// `ChannelUpdateRequest`: the five retention values always; the rest only when given
    /// (explicit presence on the wire: absent = unchanged).
    UpdateChannel {
        server: ServerId,
        channel: ChannelId,
        retention: [i32; 5],
        voice_bitrate_kbps: Option<i32>,
        slowmode_seconds: Option<i32>,
        topic: Option<String>,
    },
    ListOverwrites { server: ServerId, channel: ChannelId },
    SetOverwrite { server: ServerId, channel: ChannelId, target_type: &'static str, target_id: String, target_uid: i64, allow: u64, deny: u64 },
    SetCategorySync { server: ServerId, channel: ChannelId, enabled: bool },
    SyncChannelPermissions { server: ServerId, channel: ChannelId, category: Option<ChannelId> },
    WipeChannel { server: ServerId, channel: ChannelId },
    DeleteChannel { server: ServerId, channel: ChannelId },
    /// The channel list's create dialog is the shell's; the self-tests create their own channel
    /// with this.
    CreateChannel { server: ServerId, name: String, kind: &'static str, category: Option<ChannelId>, encrypted: bool },
}

/// The answer to "was it sent?": most calls only queue a packet; the server's answer is an
/// event.
pub type Sent = Result<(), UiError>;

/// Sees every request first: `Some(result)` answers it here (nothing reaches the core),
/// `None` lets it through.
pub type Requester = Rc<dyn Fn(&Request, &mut App) -> Option<Sent>>;

struct Installed(Requester);

impl Global for Installed {}

/// Put `requester` in front of every request (the screens' fixture, a test's recorder).
pub fn install(requester: Requester, cx: &mut App) {
    cx.set_global(Installed(requester));
}

/// Take it away again.
pub fn uninstall(cx: &mut App) {
    if cx.has_global::<Installed>() {
        cx.remove_global::<Installed>();
    }
}

/// Send `request`; `done` gets whether it went out (always after `send` returned).
pub fn send(request: Request, cx: &mut App, done: impl FnOnce(Sent, &mut App) + 'static) {
    if let Some(Installed(requester)) = cx.try_global::<Installed>() {
        let requester = requester.clone();
        if let Some(result) = requester(&request, cx) {
            cx.defer(move |cx| done(result, cx));
            return;
        }
    }
    real(request, cx, done);
}

/// Send and forget (failures are logged): the requests whose answer is only an event the view
/// already listens for.
pub fn fire(request: Request, cx: &mut App) {
    let what = name(&request);
    send(request, cx, move |result, _| {
        if let Err(e) = result {
            log::warn!("[settings] {what}: {e}");
        }
    });
}

fn name(request: &Request) -> &'static str {
    match request {
        Request::ListBans { .. } => "list_bans",
        Request::ListRoles { .. } => "list_roles",
        Request::SearchMembers { .. } => "search_members",
        Request::UpdateServer { .. } => "update_server",
        Request::TransferOwnership { .. } => "transfer_ownership",
        Request::UpdateServerPicture { .. } => "update_server_picture",
        Request::LeaveServer { .. } => "leave_server",
        Request::KickMember { .. } => "kick_member",
        Request::BanMember { .. } => "ban_member",
        Request::TimeoutMember { .. } => "timeout_member",
        Request::SetNickname { .. } => "set_nickname",
        Request::SetMemberRoles { .. } => "set_member_roles",
        Request::CreateRole { .. } => "create_role",
        Request::UpdateRole { .. } => "update_role",
        Request::DeleteRole { .. } => "delete_role",
        Request::UnbanMember { .. } => "unban_member",
        Request::ListAuditLog { .. } => "list_audit_log",
        Request::StorageInfo { .. } => "get_storage_info",
        Request::SetStorageMinFree { .. } => "set_storage_min_free",
        Request::RenameChannel { .. } => "rename_channel",
        Request::UpdateChannel { .. } => "update_channel_retention",
        Request::ListOverwrites { .. } => "list_channel_overwrites",
        Request::SetOverwrite { .. } => "set_channel_overwrite",
        Request::SetCategorySync { .. } => "set_category_sync",
        Request::SyncChannelPermissions { .. } => "sync_channel_permissions",
        Request::WipeChannel { .. } => "wipe_channel_history",
        Request::DeleteChannel { .. } => "delete_channel",
        Request::CreateChannel { .. } => "create_channel",
    }
}

fn real(request: Request, cx: &mut App, done: impl FnOnce(Sent, &mut App) + 'static) {
    use api::channels as ch;
    use api::community as co;
    let opt = |s: String| Some(s);
    match request {
        Request::ListBans { server } => call(cx, co::list_bans(co::ListBansArgs { server_id: server.wire() }), done),
        Request::ListRoles { server } => call(cx, co::list_roles(co::ListRolesArgs { server_id: server.wire() }), done),
        Request::SearchMembers { server, query, role_id, after, limit, seq } => call(
            cx,
            co::search_members(co::SearchMembersArgs {
                server_id: server.wire(),
                query: Some(query),
                role_id: Some(role_id),
                after: Some(after),
                limit: Some(limit),
                seq: Some(seq),
            }),
            done,
        ),
        Request::UpdateServer { server, name, description, public_listing } => call(
            cx,
            co::update_server(co::UpdateServerArgs { server_id: server.wire(), name, description, public_listing }),
            done,
        ),
        Request::TransferOwnership { server, new_owner, new_owner_uid } => call(
            cx,
            co::transfer_ownership(co::TransferOwnershipArgs { server_id: server.wire(), new_owner, new_owner_uid }),
            done,
        ),
        Request::UpdateServerPicture { server, bytes } => {
            call(cx, api::servers::update_server_picture(server.wire(), (*bytes).clone()), done)
        }
        Request::LeaveServer { server } => call(cx, co::leave_server(co::LeaveServerArgs { server_id: server.wire() }), done),
        Request::KickMember { server, username, uid, reason } => call(
            cx,
            co::kick_member(co::KickMemberArgs { server_id: server.wire(), username: opt(username), uid, reason: opt(reason) }),
            done,
        ),
        Request::BanMember { server, username, uid, reason, expires_at, delete_message_seconds } => call(
            cx,
            co::ban_member(co::BanMemberArgs {
                server_id: server.wire(),
                username: opt(username),
                uid,
                reason: opt(reason),
                expires_at: Some(expires_at),
                delete_message_seconds: Some(delete_message_seconds),
            }),
            done,
        ),
        Request::TimeoutMember { server, username, uid, until, reason } => call(
            cx,
            co::timeout_member(co::TimeoutMemberArgs { server_id: server.wire(), username: opt(username), uid, until, reason: opt(reason) }),
            done,
        ),
        Request::SetNickname { server, username, uid, nickname } => call(
            cx,
            co::set_nickname(co::SetNicknameArgs { server_id: server.wire(), username: opt(username), uid, nickname }),
            done,
        ),
        Request::SetMemberRoles { server, username, uid, role_ids } => call(
            cx,
            co::set_member_roles(co::SetMemberRolesArgs { server_id: server.wire(), username: opt(username), uid, role_ids }),
            done,
        ),
        Request::CreateRole { server, name, color, permissions, manage_each_other } => call(
            cx,
            co::create_role(co::CreateRoleArgs {
                server_id: server.wire(),
                name,
                color,
                permissions: permissions as i64,
                manage_each_other: Some(manage_each_other),
            }),
            done,
        ),
        Request::UpdateRole { server, role_id, name, color, permissions, position, manage_each_other } => call(
            cx,
            co::update_role(co::UpdateRoleArgs {
                server_id: server.wire(),
                role_id,
                name,
                color,
                permissions: permissions as i64,
                position,
                manage_each_other: Some(manage_each_other),
            }),
            done,
        ),
        Request::DeleteRole { server, role_id } => {
            call(cx, co::delete_role(co::DeleteRoleArgs { server_id: server.wire(), role_id }), done)
        }
        Request::UnbanMember { server, username, uid } => call(
            cx,
            co::unban_member(co::UnbanMemberArgs { server_id: server.wire(), username: opt(username), uid }),
            done,
        ),
        Request::ListAuditLog { server, before_id, limit, query, actions, seq } => call(
            cx,
            co::list_audit_log(co::ListAuditLogArgs {
                server_id: server.wire(),
                before_id: Some(before_id),
                limit: Some(limit),
                query: Some(query),
                actions: Some(actions),
                seq: Some(seq),
            }),
            done,
        ),
        Request::StorageInfo { server } => call(cx, co::get_storage_info(co::StorageInfoArgs { server_id: server.wire() }), done),
        Request::SetStorageMinFree { server, min_free_bytes } => call(
            cx,
            co::set_storage_min_free(co::SetStorageMinFreeArgs { server_id: server.wire(), min_free_bytes }),
            done,
        ),
        Request::RenameChannel { server, channel, name } => call(
            cx,
            ch::rename_channel(ch::RenameChannelArgs { server_id: server.wire(), channel_id: channel.to_string(), name }),
            done,
        ),
        Request::UpdateChannel { server, channel, retention, voice_bitrate_kbps, slowmode_seconds, topic } => call(
            cx,
            ch::update_channel_retention(ch::UpdateChannelRetentionArgs {
                server_id: server.wire(),
                channel_id: channel.to_string(),
                retention_days_text: retention[0],
                retention_days_image: retention[1],
                retention_days_video: retention[2],
                retention_days_document: retention[3],
                retention_days_audio: retention[4],
                voice_bitrate_kbps,
                slowmode_seconds,
                topic,
            }),
            done,
        ),
        Request::ListOverwrites { server, channel } => call(
            cx,
            ch::list_channel_overwrites(ch::ListChannelOverwritesArgs { server_id: server.wire(), channel_id: channel.to_string() }),
            done,
        ),
        Request::SetOverwrite { server, channel, target_type, target_id, target_uid, allow, deny } => call(
            cx,
            ch::set_channel_overwrite(ch::SetChannelOverwriteArgs {
                server_id: server.wire(),
                channel_id: channel.to_string(),
                target_type: target_type.to_string(),
                target_id,
                target_uid: Some(target_uid),
                allow: allow as i64,
                deny: deny as i64,
            }),
            done,
        ),
        Request::SetCategorySync { server, channel, enabled } => call(
            cx,
            ch::set_category_sync(ch::SetCategorySyncArgs { server_id: server.wire(), channel_id: channel.to_string(), enabled }),
            done,
        ),
        Request::SyncChannelPermissions { server, channel, category } => call(
            cx,
            ch::sync_channel_permissions(ch::SyncChannelPermissionsArgs {
                server_id: server.wire(),
                channel_id: channel.to_string(),
                category_id: category.map(|c| c.to_string()),
            }),
            done,
        ),
        Request::WipeChannel { server, channel } => call(
            cx,
            ch::wipe_channel_history(ch::WipeChannelHistoryArgs { server_id: server.wire(), channel_id: channel.to_string() }),
            done,
        ),
        Request::DeleteChannel { server, channel } => call(
            cx,
            ch::delete_channel(ch::DeleteChannelArgs { server_id: server.wire(), channel_id: channel.to_string() }),
            done,
        ),
        Request::CreateChannel { server, name, kind, category, encrypted } => call(
            cx,
            ch::create_channel(ch::CreateChannelArgs {
                server_id: server.wire(),
                name,
                channel_type: kind.to_string(),
                voice_bitrate_kbps: 0,
                category_id: category.map(|c| c.to_string()),
                encrypted: Some(encrypted),
            }),
            done,
        ),
    }
}
