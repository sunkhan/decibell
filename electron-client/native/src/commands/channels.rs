//! `#[napi]` wrappers over `decibell_core::api::channels`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::channels::{
    CreateChannelArgs, DeleteChannelArgs, DeleteChannelMessageArgs, EditChannelMessageArgs,
    ListChannelOverwritesArgs, RenameChannelArgs, ReorderChannelsArgs, RequestChannelHistoryArgs,
    SearchMessagesArgs, SendChannelMessageArgs, SetCategorySyncArgs, SetChannelOverwriteArgs,
    SyncChannelPermissionsArgs, UpdateChannelRetentionArgs, WipeChannelHistoryArgs,
};

#[napi]
pub async fn request_channel_history(args: RequestChannelHistoryArgs) -> napi::Result<()> {
    api::channels::request_channel_history(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn search_messages(args: SearchMessagesArgs) -> napi::Result<()> {
    api::channels::search_messages(args)
        .await
        .map_err(napi::Error::from)
}

/// Owner-only: nuke every message and attachment in `channel_id`.
/// Server validates ownership, applies the wipe, replies with a
/// CHANNEL_WIPE_RES (deleted counts) and broadcasts a CHANNEL_WIPED to
/// every member so their local state drops the channel's history
/// without re-fetching. The IPC returns immediately after the packet
/// is queued — the result lands later as the `channel_wipe_responded`
/// bus event.
#[napi]
pub async fn wipe_channel_history(args: WipeChannelHistoryArgs) -> napi::Result<()> {
    api::channels::wipe_channel_history(args)
        .await
        .map_err(napi::Error::from)
}

/// MANAGE_CHANNELS edit. All five retention values are sent as a
/// snapshot; 0 means "keep forever". Voice bitrate rides along when
/// provided.
#[napi]
pub async fn update_channel_retention(args: UpdateChannelRetentionArgs) -> napi::Result<()> {
    api::channels::update_channel_retention(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn send_channel_message(args: SendChannelMessageArgs) -> napi::Result<()> {
    api::channels::send_channel_message(args)
        .await
        .map_err(napi::Error::from)
}

/// Sends MESSAGE_DELETE_REQ over the community session for server_id.
/// The ack arrives as the `channel_message_delete_responded` event;
/// the broadcast (if successful) arrives as `channel_message_deleted`.
/// Server-side handler enforces self-or-can_delete_others.
#[napi]
pub async fn delete_channel_message(args: DeleteChannelMessageArgs) -> napi::Result<()> {
    api::channels::delete_channel_message(args)
        .await
        .map_err(napi::Error::from)
}

/// Sends MESSAGE_EDIT_REQ over the community session. The ack arrives as
/// `channel_message_edit_responded`; on success the broadcast arrives as
/// `channel_message_edited`. Server enforces own-message-only.
#[napi]
pub async fn edit_channel_message(args: EditChannelMessageArgs) -> napi::Result<()> {
    api::channels::edit_channel_message(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn create_channel(args: CreateChannelArgs) -> napi::Result<()> {
    api::channels::create_channel(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn reorder_channels(args: ReorderChannelsArgs) -> napi::Result<()> {
    api::channels::reorder_channels(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn rename_channel(args: RenameChannelArgs) -> napi::Result<()> {
    api::channels::rename_channel(args)
        .await
        .map_err(napi::Error::from)
}

/// Permissions v2: set (or, with allow == deny == 0, clear) one
/// per-channel overwrite. Result arrives as channel_action_responded with
/// action "overwrite"; the server then re-pushes channel_list_updated
/// (with refreshed myPermissions) and channel_overwrites_received.
#[napi]
pub async fn set_channel_overwrite(args: SetChannelOverwriteArgs) -> napi::Result<()> {
    api::channels::set_channel_overwrite(args)
        .await
        .map_err(napi::Error::from)
}

/// Permissions v3: drop the channel's own overwrites so it follows its
/// category again. Result arrives as channel_action_responded with action
/// "sync"; the server re-pushes the channel list and the overwrites.
#[napi]
pub async fn sync_channel_permissions(args: SyncChannelPermissionsArgs) -> napi::Result<()> {
    api::channels::sync_channel_permissions(args)
        .await
        .map_err(napi::Error::from)
}

/// Permissions v3: whether channels can follow this category's overwrites
/// (off = it only groups channels). Neither direction changes what any
/// channel allows. Result: channel_action_responded, action "category_sync".
#[napi]
pub async fn set_category_sync(args: SetCategorySyncArgs) -> napi::Result<()> {
    api::channels::set_category_sync(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn list_channel_overwrites(args: ListChannelOverwritesArgs) -> napi::Result<()> {
    api::channels::list_channel_overwrites(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn delete_channel(args: DeleteChannelArgs) -> napi::Result<()> {
    api::channels::delete_channel(args)
        .await
        .map_err(napi::Error::from)
}
