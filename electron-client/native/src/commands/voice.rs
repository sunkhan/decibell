//! `#[napi]` wrappers over `decibell_core::api::voice`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::voice::{
    JoinVoiceChannelArgs, SetAecEnabledArgs, SetAgcEnabledArgs, SetInputDeviceArgs,
    SetInputLevelReportingArgs, SetNoiseSuppressionLevelArgs, SetOutputDeviceArgs,
    SetSeparateStreamOutputArgs, SetStreamOutputDeviceArgs, SetStreamStereoArgs,
    SetStreamVolumeArgs, SetUserVolumeArgs, SetVoiceBitrateArgs, SetVoiceDeafenArgs,
    SetVoiceMuteArgs, SetVoiceThresholdArgs,
};

#[napi]
pub async fn join_voice_channel(args: JoinVoiceChannelArgs) -> napi::Result<()> {
    api::voice::join_voice_channel(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn leave_voice_channel() -> napi::Result<()> {
    api::voice::leave_voice_channel()
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_voice_mute(args: SetVoiceMuteArgs) -> napi::Result<()> {
    api::voice::set_voice_mute(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_voice_deafen(args: SetVoiceDeafenArgs) -> napi::Result<()> {
    api::voice::set_voice_deafen(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_voice_threshold(args: SetVoiceThresholdArgs) -> napi::Result<()> {
    api::voice::set_voice_threshold(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_voice_bitrate(args: SetVoiceBitrateArgs) -> napi::Result<()> {
    api::voice::set_voice_bitrate(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_stream_volume(args: SetStreamVolumeArgs) -> napi::Result<()> {
    api::voice::set_stream_volume(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_stream_stereo(args: SetStreamStereoArgs) -> napi::Result<()> {
    api::voice::set_stream_stereo(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_user_volume(args: SetUserVolumeArgs) -> napi::Result<()> {
    api::voice::set_user_volume(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_aec_enabled(args: SetAecEnabledArgs) -> napi::Result<()> {
    api::voice::set_aec_enabled(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_noise_suppression_level(args: SetNoiseSuppressionLevelArgs) -> napi::Result<()> {
    api::voice::set_noise_suppression_level(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_agc_enabled(args: SetAgcEnabledArgs) -> napi::Result<()> {
    api::voice::set_agc_enabled(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_input_device(args: SetInputDeviceArgs) -> napi::Result<()> {
    api::voice::set_input_device(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_output_device(args: SetOutputDeviceArgs) -> napi::Result<()> {
    api::voice::set_output_device(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_separate_stream_output(args: SetSeparateStreamOutputArgs) -> napi::Result<()> {
    api::voice::set_separate_stream_output(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_stream_output_device(args: SetStreamOutputDeviceArgs) -> napi::Result<()> {
    api::voice::set_stream_output_device(args)
        .await
        .map_err(napi::Error::from)
}

/// Gate the `voice_input_level` event (~16 Hz while in voice). Only the
/// Settings → Audio meter listens, so it switches reporting on while
/// mounted and off on unmount; default off. Sync: one atomic store.
#[napi]
pub fn set_input_level_reporting(args: SetInputLevelReportingArgs) {
    api::voice::set_input_level_reporting(args)
}
