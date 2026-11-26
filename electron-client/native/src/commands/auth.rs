//! `#[napi]` wrappers over `decibell_core::api::auth`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::auth::{
    FetchAvatarArgs, LoginArgs, RegisterArgs, RenameAccountArgs, RenameAccountResult,
    UploadAvatarResult,
};

// The two structs with a JS Buffer stay here (the core's take / give plain
// bytes); every other argument and result struct is the core's.

#[napi(object)]
pub struct UploadAvatarArgs {
    pub jpeg: napi::bindgen_prelude::Buffer,
}

#[napi(object)]
pub struct FetchAvatarResult {
    pub version: String,
    /// Empty Buffer when version == '' (no avatar).
    pub data: napi::bindgen_prelude::Buffer,
}

#[napi]
pub async fn login(args: LoginArgs) -> napi::Result<()> {
    api::auth::login(args).await.map_err(napi::Error::from)
}

/// Sign in with the saved credentials, if there are any (the renderer
/// asks at boot when `load_config` reported a `saved_username`). The
/// password stays in native. Ok(false) = nothing saved (or it doesn't
/// open on this machine any more): show the sign-in page.
#[napi]
pub async fn auto_login() -> napi::Result<bool> {
    api::auth::auto_login().await.map_err(napi::Error::from)
}

/// Change our username (uid design, stage 5). Central checks the password
/// and the name, and reserves the old name for this account for 14 days.
/// On success the saved credentials follow the new name, the renderer
/// hears `user_renamed {self: true}`, and the fresh token replaces ours and
/// goes to every community session (net/token.rs), which takes the new
/// name in place: no reconnect, so voice and streams carry on (the media
/// plane identifies people by uid).
#[napi]
pub async fn rename_account(args: RenameAccountArgs) -> napi::Result<RenameAccountResult> {
    api::auth::rename_account(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn register(args: RegisterArgs) -> napi::Result<()> {
    api::auth::register(args).await.map_err(napi::Error::from)
}

#[napi]
pub async fn logout() -> napi::Result<()> {
    api::auth::logout().await.map_err(napi::Error::from)
}

/// Upload or remove the authenticated user's avatar. Empty `jpeg`
/// argument = remove. Returns the server-computed sha256-hex version
/// on success.
#[napi]
pub async fn upload_avatar(args: UploadAvatarArgs) -> napi::Result<UploadAvatarResult> {
    api::auth::upload_avatar(args.jpeg)
        .await
        .map_err(napi::Error::from)
}

/// Fetch a specific user's avatar bytes + current version. Empty
/// version + empty data means the user has no avatar (or doesn't
/// exist — same response shape).
#[napi]
pub async fn fetch_avatar(args: FetchAvatarArgs) -> napi::Result<FetchAvatarResult> {
    let res = api::auth::fetch_avatar(args).await?;
    Ok(FetchAvatarResult {
        version: res.version,
        data: res.data.into(),
    })
}
