//! `#[napi]` wrappers over `decibell_core::api::stream_audio`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::stream_audio::{
    ListStreamAudioAppsArgs, SetStreamAudioFilterArgs, StreamAudioAppList,
};

#[napi]
pub async fn list_stream_audio_apps(
    args: ListStreamAudioAppsArgs,
) -> napi::Result<StreamAudioAppList> {
    api::stream_audio::list_stream_audio_apps(args)
        .await
        .map_err(napi::Error::from)
}

/// Store the streamer's app filter and apply it to the running share-audio
/// capture, if any. Always succeeds: without a live engine the filter is
/// simply remembered for the next `start_screen_share`.
#[napi]
pub async fn set_stream_audio_filter(args: SetStreamAudioFilterArgs) -> napi::Result<()> {
    api::stream_audio::set_stream_audio_filter(args)
        .await
        .map_err(napi::Error::from)
}
