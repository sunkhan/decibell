//! `#[napi]` wrappers over `decibell_core::api::dm`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::dm::{
    DeleteDmMessageArgs, EditDmMessageArgs, MarkDmReadArgs, RequestDmHistoryArgs,
};

#[napi]
pub async fn request_dm_conversations() -> napi::Result<()> {
    api::dm::request_dm_conversations()
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn request_dm_history(args: RequestDmHistoryArgs) -> napi::Result<()> {
    api::dm::request_dm_history(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn mark_dm_read(args: MarkDmReadArgs) -> napi::Result<()> {
    api::dm::mark_dm_read(args).await.map_err(napi::Error::from)
}

/// Sends DM_DELETE_REQ over the JWT-authed central session. The
/// ack arrives as the `dm_message_delete_responded` event; the
/// broadcast (if successful) arrives as `dm_message_deleted`. Both
/// land in useDmEvents on the renderer side.
#[napi]
pub async fn delete_dm_message(args: DeleteDmMessageArgs) -> napi::Result<()> {
    api::dm::delete_dm_message(args)
        .await
        .map_err(napi::Error::from)
}

/// Sends DM_EDIT_REQ over the JWT-authed central session. The ack arrives as
/// `dm_message_edit_responded`; on success the broadcast arrives as
/// `dm_message_edited`. Central enforces own-message-only.
#[napi]
pub async fn edit_dm_message(args: EditDmMessageArgs) -> napi::Result<()> {
    api::dm::edit_dm_message(args)
        .await
        .map_err(napi::Error::from)
}
