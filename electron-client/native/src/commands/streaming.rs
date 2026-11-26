//! `#[napi]` wrappers over `decibell_core::api::streaming`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
#[cfg(any(target_os = "windows", target_os = "linux"))]
use crate::api::streaming::NativeEncoderCap;
use crate::api::streaming::{
    CapsResponse, CodecSettingsValue, FetchStreamThumbnailArgs, MoveStreamToChannelArgs,
    RequestStreamKeyframeArgs, SetDecoderCapsArgs, SetEncoderCapsArgs, SetStreamFrameSinkArgs,
    StartScreenShareArgs, StartScreenShareResult, StopScreenShareArgs, StopWatchingArgs,
    WatchStreamArgs,
};

// The structs with a JS Buffer stay here (the core takes / gives plain
// bytes); every other argument and result struct is the core's.

/// PR8 hot path: renderer's WebCodecs.VideoEncoder produces encoded
/// chunks; we packetise + UDP. Called once per encoded frame at the
/// configured FPS — needs to be cheap.
#[napi(object)]
pub struct SendVideoFrameArgs {
    /// 1=H264_HW, 2=H264_SW, 3=H265, 4=AV1.
    pub codec: u8,
    pub keyframe: bool,
    /// Encoded chunk bytes. Typed as `Buffer` (not `Vec<u8>`) so napi-rs
    /// accepts a JS `Uint8Array` directly without copying the bytes
    /// through V8's array-conversion path. The renderer sends one of
    /// these per encoded frame, so the per-frame allocation savings
    /// matter.
    pub data: napi::bindgen_prelude::Buffer,
    /// hvcC / av1C / avcC bytes for keyframes when the encoder produces
    /// them out-of-band (WebCodecs `metadata.decoderConfig.description`).
    /// Receivers use this directly to configure WebCodecs.VideoDecoder.
    /// Optional — for H.264 in Annex B with inline SPS/PPS, the decoder
    /// reads from the bitstream itself.
    pub description: Option<napi::bindgen_prelude::Buffer>,
}

#[napi(object)]
pub struct FetchStreamThumbnailResult {
    pub uid: i64,
    /// Empty Buffer when the server has no cached thumbnail yet
    /// (stream just started, or fetch arrived between frames).
    pub jpeg: napi::bindgen_prelude::Buffer,
}

#[napi(object)]
pub struct SendStreamThumbnailArgs {
    pub server_id: String,
    pub channel_id: String,
    /// JPEG bytes. Typed as `Buffer` (not `Vec<u8>`) so napi-rs
    /// accepts a JS `Uint8Array` directly without forcing the
    /// renderer to materialise a plain Array — `Vec<u8>` would
    /// reject the typed-array shape with "not an array".
    pub jpeg_data: napi::bindgen_prelude::Buffer,
}

#[napi]
pub async fn start_screen_share(
    args: StartScreenShareArgs,
) -> napi::Result<StartScreenShareResult> {
    api::streaming::start_screen_share(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn stop_screen_share(args: StopScreenShareArgs) -> napi::Result<()> {
    api::streaming::stop_screen_share(args)
        .await
        .map_err(napi::Error::from)
}

/// Carry an already-running stream into a new voice channel WITHOUT restarting
/// capture/encode. The caller has already joined the new voice channel, so a
/// fresh VoiceEngine with new UDP sockets exists, and a new MLS group whose
/// keys are the only ones the new channel can open; here we re-point the
/// video (and stream-audio) senders at those sockets and that key ring, and
/// re-announce the stream in the new channel. Because capture keeps running
/// there is no portal/WGC re-prompt — the whole reason this exists instead of
/// stop+start.
#[napi]
pub async fn move_stream_to_channel(args: MoveStreamToChannelArgs) -> napi::Result<()> {
    api::streaming::move_stream_to_channel(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub fn send_video_frame(args: SendVideoFrameArgs) -> napi::Result<()> {
    let description: Option<&[u8]> = args.description.as_ref().map(|d| d.as_ref());
    api::streaming::send_video_frame(args.codec, args.keyframe, args.data.as_ref(), description)
        .map_err(napi::Error::from)
}

/// Renderer-side stream-frame subscriptions, mirrored natively. The preload
/// bridge calls this when the first subscriber for a media key appears
/// (enabled) and when the last one goes (disabled); frames for keys
/// without a sink are dropped before the TSFN — and own-stream self-preview
/// isn't even copied. Sync, and sent over the same `decibell:invoke`
/// channel as the player's follow-up keyframe request, so the sink is in
/// place before that keyframe can arrive. NOT the watch filter
/// (media::WATCHED_STREAMS) — that one gates the server-side watch.
#[napi]
pub fn set_stream_frame_sink(args: SetStreamFrameSinkArgs) {
    api::streaming::set_stream_frame_sink(args)
}

/// Drop every stream-frame sink. The preload calls this when it loads (a
/// renderer reload leaves no subscribers behind).
#[napi]
pub fn clear_stream_frame_sinks() {
    api::streaming::clear_stream_frame_sinks()
}

#[napi]
pub async fn watch_stream(args: WatchStreamArgs) -> napi::Result<()> {
    api::streaming::watch_stream(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn stop_watching(args: StopWatchingArgs) -> napi::Result<()> {
    api::streaming::stop_watching(args)
        .await
        .map_err(napi::Error::from)
}

/// On-demand fetch of the latest thumbnail for a streaming user.
/// Called by the renderer when UserProfilePopup opens for a user
/// known to be streaming. Returns empty bytes when no frame is
/// cached yet — caller renders the gradient placeholder.
///
/// Same shape as `fetch_avatar` in commands/auth.rs: single-slot
/// oneshot per streamer (by uid), 5-second timeout, slot cleanup on timeout.
#[napi]
pub async fn fetch_stream_thumbnail(
    args: FetchStreamThumbnailArgs,
) -> napi::Result<FetchStreamThumbnailResult> {
    let res = api::streaming::fetch_stream_thumbnail(args).await?;
    Ok(FetchStreamThumbnailResult {
        uid: res.uid,
        jpeg: res.jpeg.into(),
    })
}

#[napi]
pub async fn get_caps() -> napi::Result<CapsResponse> {
    api::streaming::get_caps().await.map_err(napi::Error::from)
}

#[napi]
pub async fn set_encoder_caps(args: SetEncoderCapsArgs) -> napi::Result<()> {
    api::streaming::set_encoder_caps(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_decoder_caps(args: SetDecoderCapsArgs) -> napi::Result<()> {
    api::streaming::set_decoder_caps(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn get_codec_settings() -> napi::Result<CodecSettingsValue> {
    api::streaming::get_codec_settings()
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_codec_settings(args: CodecSettingsValue) -> napi::Result<()> {
    api::streaming::set_codec_settings(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn send_stream_thumbnail(args: SendStreamThumbnailArgs) -> napi::Result<()> {
    api::streaming::send_stream_thumbnail(args.server_id, args.channel_id, args.jpeg_data)
        .await
        .map_err(napi::Error::from)
}

/// Runs the native FFmpeg encoder probe. Windows-only. Returns the
/// list of (codec, vendor) tuples that successfully opened.
///
/// Async + spawn_blocking: the probe opens (and closes) FFmpeg hardware
/// encoders, which takes hundreds of ms — as a sync napi fn it ran on the
/// Electron main thread and froze every window for the duration.
#[cfg(target_os = "windows")]
#[napi]
pub async fn probe_native_encoders() -> napi::Result<Vec<NativeEncoderCap>> {
    api::streaming::probe_native_encoders()
        .await
        .map_err(napi::Error::from)
}

/// Async + spawn_blocking for the same reason as the Windows variant: the
/// test-opens must not run on the Electron main thread.
#[cfg(target_os = "linux")]
#[napi]
pub async fn probe_native_encoders() -> napi::Result<Vec<NativeEncoderCap>> {
    api::streaming::probe_native_encoders()
        .await
        .map_err(napi::Error::from)
}

/// Force the next encoded frame on the active stream (if any) to be a
/// keyframe. Wired from the renderer's `keyframe_requested` event.
/// On Linux/macOS this is a no-op stub — the renderer's WebCodecs
/// encoder handles keyframe forcing in JS via VideoEncoder.encode's
/// `keyFrame: true` option.
#[napi]
pub async fn force_keyframe() -> napi::Result<()> {
    api::streaming::force_keyframe()
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn request_stream_keyframe(args: RequestStreamKeyframeArgs) -> napi::Result<()> {
    api::streaming::request_stream_keyframe(args)
        .await
        .map_err(napi::Error::from)
}
