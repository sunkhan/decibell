//! `#[napi]` wrappers over `decibell_core::api::messaging`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::messaging::SendPrivateMessageArgs;

#[napi]
pub async fn send_private_message(args: SendPrivateMessageArgs) -> napi::Result<()> {
    api::messaging::send_private_message(args)
        .await
        .map_err(napi::Error::from)
}
