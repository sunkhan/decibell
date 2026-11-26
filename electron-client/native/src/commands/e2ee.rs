//! `#[napi]` wrappers over `decibell_core::api::e2ee`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.
//!
//! `js_name` is explicit on every command: napi's camel-casing splits on
//! the digit (`e2ee_get_status` → `e2EeGetStatus`), which the renderer's
//! `invoke()` normaliser (`_x` → `X`) would never produce. The same split
//! names the result interfaces `E2EeStatus` / `E2EePeerInfo`; napi resolves
//! a struct's JS name only inside the crate that defines it (the core), so
//! the wrappers here state their return types.

use crate::api;
use crate::api::e2ee::{E2eePeerInfo, E2eeStatus, PassphraseArgs, PeerInfoArgs};

#[napi(js_name = "e2eeGetStatus", ts_return_type = "Promise<E2EeStatus>")]
pub async fn e2ee_get_status() -> napi::Result<E2eeStatus> {
    api::e2ee::e2ee_get_status()
        .await
        .map_err(napi::Error::from)
}

#[napi(js_name = "e2eeSetup")]
pub async fn e2ee_setup(args: PassphraseArgs) -> napi::Result<()> {
    api::e2ee::e2ee_setup(args).await.map_err(napi::Error::from)
}

#[napi(js_name = "e2eeUnlock")]
pub async fn e2ee_unlock(args: PassphraseArgs) -> napi::Result<()> {
    api::e2ee::e2ee_unlock(args)
        .await
        .map_err(napi::Error::from)
}

#[napi(js_name = "e2eeChangePassphrase")]
pub async fn e2ee_change_passphrase(args: PassphraseArgs) -> napi::Result<()> {
    api::e2ee::e2ee_change_passphrase(args)
        .await
        .map_err(napi::Error::from)
}

#[napi(js_name = "e2eeReset")]
pub async fn e2ee_reset(args: PassphraseArgs) -> napi::Result<()> {
    api::e2ee::e2ee_reset(args).await.map_err(napi::Error::from)
}

#[napi(js_name = "e2eePeerInfo", ts_return_type = "Promise<E2EePeerInfo>")]
pub async fn e2ee_peer_info(args: PeerInfoArgs) -> napi::Result<E2eePeerInfo> {
    api::e2ee::e2ee_peer_info(args)
        .await
        .map_err(napi::Error::from)
}
