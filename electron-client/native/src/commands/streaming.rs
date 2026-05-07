//! Streaming commands (PR8 — post-FFmpeg-removal shape).
//!
//! Capture + encode live in the renderer (Chromium's `getDisplayMedia`
//! + `WebCodecs.VideoEncoder`). Native side handles:
//!  - Wire signalling: StartStreamReq, StopStreamReq, WatchStreamReq,
//!    StopWatchingReq, StreamThumbnailUpdate.
//!  - Packetisation: `send_video_frame` accepts encoded chunks from
//!    the renderer and emits them onto the media UDP socket via
//!    `VideoEngine`.
//!  - Caps storage: encode + decode caps come from the renderer's
//!    WebCodecs probes (`set_encoder_caps`, `set_decoder_caps`); they
//!    live on AppState and feed JoinVoiceRequest's ClientCapabilities.

use crate::media::caps::{CodecCap, CodecKind};
use crate::media::VideoEngine;
use crate::net::connection::build_packet;
use crate::net::proto::{packet, *};
use crate::state;

#[napi(object)]
pub struct StartScreenShareArgs {
    pub server_id: String,
    pub channel_id: String,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub video_bitrate_kbps: u32,
    pub share_audio: bool,
    pub audio_bitrate_kbps: u32,
    /// VideoCodec byte: 1=H264_HW, 2=H264_SW, 3=H265, 4=AV1.
    /// 0 means "no enforcement, LCD picker chooses initial codec".
    pub initial_codec: u8,
    /// 0 = no enforcement, otherwise locks the stream to this codec.
    pub enforced_codec: u8,
}

#[napi]
pub async fn start_screen_share(args: StartScreenShareArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let mut s = state_arc.lock().await;
        if s.video_engine.is_some() {
            return Err(napi::Error::from_reason("Already sharing screen"));
        }
        let voice = s.voice_engine.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Must be in a voice channel to share screen")
        })?;
        let media_socket = voice.media_socket();
        let sender_id = voice.sender_id().to_string();
        let self_username = s
            .username
            .clone()
            .ok_or_else(|| napi::Error::from_reason("Not authenticated"))?;

        let client = s.communities.get(&args.server_id).ok_or_else(|| {
            napi::Error::from_reason(format!(
                "Not connected to community {}",
                args.server_id
            ))
        })?;
        let tx = client.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Community connection lost")
        })?;
        let pkt = build_packet(
            packet::Type::StartStreamReq,
            packet::Payload::StartStreamReq(StartStreamRequest {
                channel_id: args.channel_id.clone(),
                target_fps: args.fps as i32,
                target_bitrate_kbps: args.video_bitrate_kbps as i32,
                has_audio: args.share_audio,
                resolution_width: args.width,
                resolution_height: args.height,
                chosen_codec: args.initial_codec as i32,
                enforced_codec: args.enforced_codec as i32,
            }),
            Some(&client.jwt),
        );

        s.video_engine = Some(VideoEngine::start(media_socket, sender_id, self_username));
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct StopScreenShareArgs {
    pub server_id: String,
    pub channel_id: String,
}

#[napi]
pub async fn stop_screen_share(args: StopScreenShareArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let mut s = state_arc.lock().await;
        s.video_engine = None;
        s.audio_stream_engine = None;
        let client = s.communities.get(&args.server_id).ok_or_else(|| {
            napi::Error::from_reason(format!(
                "Not connected to community {}",
                args.server_id
            ))
        })?;
        let tx = client.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Community connection lost")
        })?;
        let pkt = build_packet(
            packet::Type::StopStreamReq,
            packet::Payload::StopStreamReq(StopStreamRequest {
                channel_id: args.channel_id,
            }),
            Some(&client.jwt),
        );
        (tx, pkt)
    };

    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await;
    Ok(())
}

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

#[napi]
pub async fn send_video_frame(args: SendVideoFrameArgs) -> napi::Result<()> {
    use crate::media::video_packet::WIRE_DESCRIPTION_MAGIC;

    let state_arc = state::shared();
    let s = state_arc.lock().await;
    let engine = s.video_engine.as_ref().ok_or_else(|| {
        napi::Error::from_reason("Not currently streaming")
    })?;

    let data: &[u8] = args.data.as_ref();

    // For HEVC/AV1 keyframes with a description, prepend the magic-tag
    // length-prefix so receivers strip it back out and surface
    // description as a separate field. H.264 keyframes carry SPS/PPS
    // inline in Annex B and don't need this.
    let payload: Vec<u8> = if args.keyframe && (args.codec == 3 || args.codec == 4) {
        if let Some(desc) = args.description.as_ref() {
            let desc_bytes: &[u8] = desc.as_ref();
            let mut wire = Vec::with_capacity(
                WIRE_DESCRIPTION_MAGIC.len() + 4 + desc_bytes.len() + data.len(),
            );
            wire.extend_from_slice(&WIRE_DESCRIPTION_MAGIC);
            wire.extend_from_slice(&(desc_bytes.len() as u32).to_be_bytes());
            wire.extend_from_slice(desc_bytes);
            wire.extend_from_slice(data);
            wire
        } else {
            data.to_vec()
        }
    } else {
        data.to_vec()
    };

    engine.send_encoded_frame(args.codec, args.keyframe, &payload);
    Ok(())
}

#[napi(object)]
pub struct WatchStreamArgs {
    pub server_id: String,
    pub channel_id: String,
    pub target_username: String,
}

#[napi]
pub async fn watch_stream(args: WatchStreamArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let client = s.communities.get(&args.server_id).ok_or_else(|| {
            napi::Error::from_reason(format!(
                "Not connected to community {}",
                args.server_id
            ))
        })?;
        let tx = client.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Community connection lost")
        })?;
        let pkt = build_packet(
            packet::Type::WatchStreamReq,
            packet::Payload::WatchStreamReq(WatchStreamRequest {
                channel_id: args.channel_id,
                target_username: args.target_username,
            }),
            Some(&client.jwt),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct StopWatchingArgs {
    pub server_id: String,
    pub channel_id: String,
    pub target_username: String,
}

#[napi]
pub async fn stop_watching(args: StopWatchingArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let client = s.communities.get(&args.server_id).ok_or_else(|| {
            napi::Error::from_reason(format!(
                "Not connected to community {}",
                args.server_id
            ))
        })?;
        let tx = client.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Community connection lost")
        })?;
        let pkt = build_packet(
            packet::Type::StopWatchingReq,
            packet::Payload::StopWatchingReq(StopWatchingRequest {
                channel_id: args.channel_id,
                target_username: args.target_username,
            }),
            Some(&client.jwt),
        );
        (tx, pkt)
    };

    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await;
    Ok(())
}

#[napi(object)]
pub struct CodecCapValue {
    /// 1=H264_HW, 2=H264_SW, 3=H265, 4=AV1.
    pub codec: u8,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
}

#[napi(object)]
pub struct CapsResponse {
    pub encode: Vec<CodecCapValue>,
    pub decode: Vec<CodecCapValue>,
}

fn cap_to_value(c: &CodecCap) -> CodecCapValue {
    CodecCapValue {
        codec: c.codec as u8,
        max_width: c.max_width,
        max_height: c.max_height,
        max_fps: c.max_fps,
    }
}

fn value_to_cap(v: &CodecCapValue) -> Option<CodecCap> {
    let kind = match v.codec {
        1 => CodecKind::H264Hw,
        2 => CodecKind::H264Sw,
        3 => CodecKind::H265,
        4 => CodecKind::Av1,
        _ => return None,
    };
    Some(CodecCap {
        codec: kind,
        max_width: v.max_width,
        max_height: v.max_height,
        max_fps: v.max_fps,
    })
}

#[napi]
pub async fn get_caps() -> napi::Result<CapsResponse> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    Ok(CapsResponse {
        encode: s.encoder_caps.iter().map(cap_to_value).collect(),
        decode: s.decoder_caps.iter().map(cap_to_value).collect(),
    })
}

#[napi(object)]
pub struct SetEncoderCapsArgs {
    pub encoder_caps: Vec<CodecCapValue>,
}

#[napi]
pub async fn set_encoder_caps(args: SetEncoderCapsArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let mut s = state_arc.lock().await;
    s.encoder_caps = args.encoder_caps.iter().filter_map(value_to_cap).collect();
    Ok(())
}

#[napi(object)]
pub struct SetDecoderCapsArgs {
    pub decoder_caps: Vec<CodecCapValue>,
}

#[napi]
pub async fn set_decoder_caps(args: SetDecoderCapsArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let mut s = state_arc.lock().await;
    s.decoder_caps = args.decoder_caps.iter().filter_map(value_to_cap).collect();
    Ok(())
}

/// Codec preference toggles persisted in the on-disk config. The
/// streaming UI reads these to gray out codecs the user has opted out
/// of. They also feed the toggle filter applied to encoder caps before
/// they ship in JoinVoiceRequest.
#[napi(object)]
pub struct CodecSettingsValue {
    pub use_av1: bool,
    pub use_h265: bool,
}

#[napi]
pub async fn get_codec_settings() -> napi::Result<CodecSettingsValue> {
    let settings = crate::config::load()
        .map_err(napi::Error::from_reason)?
        .settings;
    Ok(CodecSettingsValue {
        use_av1: settings.use_av1,
        use_h265: settings.use_h265,
    })
}

#[napi]
pub async fn set_codec_settings(args: CodecSettingsValue) -> napi::Result<()> {
    let mut current = crate::config::load()
        .map_err(napi::Error::from_reason)?
        .settings;
    current.use_av1 = args.use_av1;
    current.use_h265 = args.use_h265;
    crate::config::save(None, &current).map_err(napi::Error::from_reason)?;
    Ok(())
}

#[napi(object)]
pub struct SendStreamThumbnailArgs {
    pub server_id: String,
    pub channel_id: String,
    pub jpeg_data: Vec<u8>,
}

#[napi]
pub async fn send_stream_thumbnail(args: SendStreamThumbnailArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    let client = s.communities.get(&args.server_id).ok_or_else(|| {
        napi::Error::from_reason(format!(
            "Not connected to community {}",
            args.server_id
        ))
    })?;
    client
        .send_stream_thumbnail(&args.channel_id, &args.jpeg_data)
        .await
        .map_err(napi::Error::from_reason)
}
