//! `#[napi]` wrappers over `decibell_core::api::call`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::call::{
    CallConfig, CallConnectArgs, CallPrepareArgs, CallPrepareResult, CallWatchStreamArgs,
    SendCallSignalArgs,
};

#[napi]
pub async fn get_call_config() -> napi::Result<CallConfig> {
    api::call::get_call_config()
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn send_call_signal(args: SendCallSignalArgs) -> napi::Result<()> {
    api::call::send_call_signal(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn call_prepare(args: CallPrepareArgs) -> napi::Result<CallPrepareResult> {
    api::call::call_prepare(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn call_connect(args: CallConnectArgs) -> napi::Result<()> {
    api::call::call_connect(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn call_end() -> napi::Result<()> {
    api::call::call_end().await.map_err(napi::Error::from)
}

#[napi]
pub async fn call_watch_stream(args: CallWatchStreamArgs) -> napi::Result<()> {
    api::call::call_watch_stream(args)
        .await
        .map_err(napi::Error::from)
}
