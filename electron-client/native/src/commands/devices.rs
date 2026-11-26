//! `#[napi]` wrappers over `decibell_core::api::devices`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::devices::{
    ChangePasswordArgs, ChangePasswordResult, DeviceEntry, SignOutDevicesArgs, SignOutDevicesResult,
};

/// The devices signed in to the account, most recently active first.
#[napi]
pub async fn list_devices() -> napi::Result<Vec<DeviceEntry>> {
    api::devices::list_devices()
        .await
        .map_err(napi::Error::from)
}

/// Sign devices out: central closes their connections (they sign out and
/// forget their saved sign-in), their reconnect tokens stop working, and
/// communities drop their sessions within a minute.
#[napi]
pub async fn sign_out_devices(args: SignOutDevicesArgs) -> napi::Result<SignOutDevicesResult> {
    api::devices::sign_out_devices(args)
        .await
        .map_err(napi::Error::from)
}

/// Change the account's password. Central checks the current one and signs
/// every other device out; this one stays signed in with a new reconnect
/// token, and its saved sign-in (if any) takes the new password.
#[napi]
pub async fn change_password(args: ChangePasswordArgs) -> napi::Result<ChangePasswordResult> {
    api::devices::change_password(args)
        .await
        .map_err(napi::Error::from)
}
