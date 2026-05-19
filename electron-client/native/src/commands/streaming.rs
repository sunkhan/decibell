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

#[cfg(target_os = "windows")]
use crate::media::{capture_audio_wasapi, source_id as source_id_parser, AudioStreamEngine};

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
    /// Chromium desktopCapturer source id ("screen:N:0" or "window:HWND:0").
    /// Required on Windows where native opens the source via WGC.
    /// Optional on Linux/macOS where the renderer's getDisplayMedia
    /// handles the picker via xdg-desktop-portal / ScreenCaptureKit.
    pub source_id: Option<String>,
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
        // Stream audio rides the voice UDP socket — receivers demux by
        // packet type (STREAM_AUDIO = 6) in the main voice pipeline.
        // Grabbed alongside media_socket so we don't have to re-borrow
        // `voice` after `s.video_engine = ...` consumes the mutable
        // state. Sender ID gets a second clone because VideoEngine::start
        // takes ownership of the first.
        #[cfg(target_os = "windows")]
        let voice_socket = voice.voice_socket();
        let sender_id = voice.sender_id().to_string();
        #[cfg(target_os = "windows")]
        let audio_sender_id = sender_id.clone();
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

        let mut engine = VideoEngine::start(media_socket, sender_id, self_username);

        // Windows: spin up the native capture + encoder pipeline. The
        // renderer never calls send_video_frame on Windows — capture
        // and encode happen here. On Linux/macOS the renderer keeps
        // owning capture (getDisplayMedia) and encode (WebCodecs) and
        // pumps frames via send_video_frame; nothing native-side to do
        // beyond constructing VideoEngine.
        #[cfg(target_os = "windows")]
        {
            let source_id = args.source_id.as_deref().ok_or_else(|| {
                napi::Error::from_reason("source_id required on Windows")
            })?;
            // Resolve the requested codec to a working FFmpeg encoder
            // name via the native probe. initial_codec=0 means "auto" —
            // pick the first HW encoder available (whatever NVENC/AMF
            // gave us). Otherwise honour the user's pick; error if the
            // probe didn't see that codec as available.
            let caps = crate::media::encoder_probe::run(read_primary_gpu_vendor_id());
            if caps.is_empty() {
                return Err(napi::Error::from_reason(
                    "No hardware encoder available — install your GPU's video drivers",
                ));
            }
            let pick = if args.initial_codec == 0 {
                &caps[0]
            } else {
                caps.iter().find(|c| c.codec as u8 == args.initial_codec).ok_or_else(
                    || {
                        napi::Error::from_reason(format!(
                            "Codec {} not available on this hardware",
                            args.initial_codec
                        ))
                    },
                )?
            };
            engine
                .start_windows(
                    source_id,
                    &pick.encoder_name,
                    pick.codec as u8,
                    args.width,
                    args.height,
                    args.fps,
                    args.video_bitrate_kbps,
                    args.server_id.clone(),
                    args.channel_id.clone(),
                )
                .map_err(napi::Error::from_reason)?;

            // Stream audio: spin up the WASAPI capture + Opus encode +
            // packetise pipeline alongside the video engine. Branch on
            // source_id — `screen:N:0` means full-display capture (we
            // exclude our own process tree so participants don't hear
            // themselves echoed back); `window:HWND:0` means per-window
            // capture (resolve HWND → owning PID via
            // GetWindowThreadProcessId, then INCLUDE that process tree).
            //
            // Audio failures here are non-fatal: video keeps streaming,
            // the warning logs and the user gets a stream with no
            // audio. Better than the whole start failing because of an
            // audio device hiccup.
            if args.share_audio {
                let frame_rx_opt: Option<std::sync::mpsc::Receiver<crate::media::capture::AudioFrame>> =
                    match source_id_parser::parse(source_id) {
                        Ok(source_id_parser::CaptureTarget::Monitor(_)) => {
                            match capture_audio_wasapi::start_system_audio_capture() {
                                Ok(rx) => {
                                    eprintln!("[stream-audio] system loopback (exclude decibell) started");
                                    Some(rx)
                                }
                                Err(e) => {
                                    eprintln!("[stream-audio] system loopback failed: {}", e);
                                    None
                                }
                            }
                        }
                        Ok(source_id_parser::CaptureTarget::Window(hwnd_u64)) => {
                            // Resolve the window's owning process so the
                            // process-loopback target matches what the
                            // user is actually streaming.
                            let pid: Option<u32> = unsafe {
                                use windows::Win32::Foundation::HWND;
                                use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;
                                let hwnd = HWND(hwnd_u64 as *mut _);
                                let mut pid: u32 = 0;
                                let tid = GetWindowThreadProcessId(hwnd, Some(&mut pid));
                                if tid == 0 || pid == 0 { None } else { Some(pid) }
                            };
                            match pid {
                                Some(p) => match capture_audio_wasapi::start_process_audio_capture(p) {
                                    Ok(rx) => {
                                        eprintln!("[stream-audio] per-window loopback for pid={} started", p);
                                        Some(rx)
                                    }
                                    Err(e) => {
                                        eprintln!("[stream-audio] per-window loopback failed: {}", e);
                                        None
                                    }
                                },
                                None => {
                                    eprintln!("[stream-audio] could not resolve PID from HWND={}", hwnd_u64);
                                    None
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[stream-audio] source_id parse error: {:?}", e);
                            None
                        }
                    };

                if let Some(frame_rx) = frame_rx_opt {
                    let audio_engine = AudioStreamEngine::start(
                        frame_rx,
                        voice_socket,
                        audio_sender_id,
                        args.audio_bitrate_kbps,
                    );
                    s.audio_stream_engine = Some(audio_engine);
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            // source_id is ignored on Linux/macOS; renderer's
            // getDisplayMedia handles the picker there.
            let _ = args.source_id;
        }

        s.video_engine = Some(engine);
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
pub fn send_video_frame(args: SendVideoFrameArgs) -> napi::Result<()> {
    // Windows native pipeline owns encode end-to-end; the renderer
    // never pumps frames here on that platform. Kept as a no-op stub
    // so the existing renderer code can ship without `cfg(platform)`
    // guards on its call site.
    #[cfg(target_os = "windows")]
    {
        let _ = args;
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        use crate::media::video_packet::WIRE_DESCRIPTION_MAGIC;
        use crate::media::video_pipeline;

        // Hot path: read the active sender from the dedicated frame-
        // sink slot. No `state_arc.lock()` here — that mutex is
        // contended with every other tokio task that touches AppState
        // and was a real serialisation point at 60–120 fps. The slot's
        // mutex is held for ~tens of nanoseconds (clone an Arc) and
        // only contended on start/stop transitions.
        //
        // Sync rather than async — no .await anywhere in the body,
        // so napi-rs doesn't spawn a task per frame.
        let sender = video_pipeline::current_frame_sink()
            .ok_or_else(|| napi::Error::from_reason("Not currently streaming"))?;

        let data: &[u8] = args.data.as_ref();

        // For HEVC/AV1 keyframes with a description, prepend the
        // magic-tag length-prefix so receivers strip it back out and
        // surface the description as a separate field. H.264 keyframes
        // carry SPS/PPS inline in Annex B and don't need this.
        if args.keyframe && (args.codec == 3 || args.codec == 4) {
            if let Some(desc) = args.description.as_ref() {
                let desc_bytes: &[u8] = desc.as_ref();
                let mut wire = Vec::with_capacity(
                    WIRE_DESCRIPTION_MAGIC.len() + 4 + desc_bytes.len() + data.len(),
                );
                wire.extend_from_slice(&WIRE_DESCRIPTION_MAGIC);
                wire.extend_from_slice(&(desc_bytes.len() as u32).to_be_bytes());
                wire.extend_from_slice(desc_bytes);
                wire.extend_from_slice(data);
                sender.send_frame(args.codec, args.keyframe, &wire);
                return Ok(());
            }
        }
        sender.send_frame(args.codec, args.keyframe, data);
        Ok(())
    }
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

// ─── On-demand stream-thumbnail fetch (for UserPopup live preview) ──

#[napi(object)]
pub struct FetchStreamThumbnailArgs {
    pub server_id: String,
    pub username: String,
}

#[napi(object)]
pub struct FetchStreamThumbnailResult {
    pub username: String,
    /// Empty Buffer when the server has no cached thumbnail yet
    /// (stream just started, or fetch arrived between frames).
    pub jpeg: napi::bindgen_prelude::Buffer,
}

/// On-demand fetch of the latest thumbnail for a streaming user.
/// Called by the renderer when UserProfilePopup opens for a user
/// known to be streaming. Returns empty bytes when no frame is
/// cached yet — caller renders the gradient placeholder.
///
/// Same shape as `fetch_avatar` in commands/auth.rs: single-slot
/// oneshot per username, 5-second timeout, slot cleanup on timeout.
#[napi]
pub async fn fetch_stream_thumbnail(
    args: FetchStreamThumbnailArgs,
) -> napi::Result<FetchStreamThumbnailResult> {
    use tokio::sync::oneshot;

    let FetchStreamThumbnailArgs { server_id, username } = args;
    let state_arc = state::shared();

    let (write_tx, data, rx) = {
        let mut s = state_arc.lock().await;
        let client = s.communities.get(&server_id).ok_or_else(|| {
            napi::Error::from_reason(format!(
                "Not connected to community {}",
                server_id
            ))
        })?;
        let tx = client.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Community connection lost")
        })?;
        let pkt = build_packet(
            packet::Type::FetchStreamThumbnailReq,
            packet::Payload::FetchStreamThumbnailReq(FetchStreamThumbnailReq {
                owner_username: username.clone(),
            }),
            Some(&client.jwt),
        );
        let (otx, orx) = oneshot::channel();
        // Last-request-wins per username — supersedes any earlier
        // in-flight fetch (its .await will time out, harmless).
        s.pending_thumbnail_fetches.insert(username.clone(), otx);
        (tx, pkt, orx)
    };

    if tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data))
        .await
        .is_err()
    {
        state_arc.lock().await.pending_thumbnail_fetches.remove(&username);
        return Err(napi::Error::from_reason("Failed to send thumbnail fetch"));
    }

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(resp)) => Ok(FetchStreamThumbnailResult {
            username: resp.owner_username,
            jpeg: resp.thumbnail_data.into(),
        }),
        Ok(Err(_)) => Err(napi::Error::from_reason(
            "Community connection closed before thumbnail response",
        )),
        Err(_) => {
            state_arc
                .lock()
                .await
                .pending_thumbnail_fetches
                .remove(&username);
            Err(napi::Error::from_reason("Thumbnail fetch timed out"))
        }
    }
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
    /// JPEG bytes. Typed as `Buffer` (not `Vec<u8>`) so napi-rs
    /// accepts a JS `Uint8Array` directly without forcing the
    /// renderer to materialise a plain Array — `Vec<u8>` would
    /// reject the typed-array shape with "not an array".
    pub jpeg_data: napi::bindgen_prelude::Buffer,
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
        .send_stream_thumbnail(&args.channel_id, args.jpeg_data.as_ref())
        .await
        .map_err(napi::Error::from_reason)
}

// ─── Windows native FFmpeg encoder commands (PR after PR8) ─────────
//
// Replaces Chromium's WebCodecs path on Windows because Chromium's
// MFT encoder factory caps at 30 fps. See the design spec at
// docs/superpowers/specs/2026-05-12-windows-native-ffmpeg-encoder-design.md
// for the full motivation. Linux/macOS continue to use the WebCodecs
// path and these commands are stubbed out (or absent) there.

/// Native encoder capability returned by `probe_native_encoders`.
/// Same shape the renderer's WebCodecs probe used to populate.
#[cfg(target_os = "windows")]
#[napi(object)]
pub struct NativeEncoderCap {
    /// VideoCodec wire id (1=H264_HW, 3=H265, 4=AV1).
    pub codec: i32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub hardware: bool,
    /// FFmpeg encoder name that actually opens (e.g. "h264_nvenc").
    pub encoder_name: String,
}

/// Runs the native FFmpeg encoder probe. Windows-only. Returns the
/// list of (codec, vendor) tuples that successfully opened.
#[cfg(target_os = "windows")]
#[napi]
pub fn probe_native_encoders() -> napi::Result<Vec<NativeEncoderCap>> {
    let vendor_id = read_primary_gpu_vendor_id();
    let caps = crate::media::encoder_probe::run(vendor_id);
    Ok(caps
        .into_iter()
        .map(|c| NativeEncoderCap {
            codec: c.codec,
            max_width: c.max_width,
            max_height: c.max_height,
            max_fps: c.max_fps,
            hardware: c.hardware,
            encoder_name: c.encoder_name,
        })
        .collect())
}

/// Force the next encoded frame on the active stream (if any) to be a
/// keyframe. Wired from the renderer's `keyframe_requested` event.
/// On Linux/macOS this is a no-op stub — the renderer's WebCodecs
/// encoder handles keyframe forcing in JS via VideoEncoder.encode's
/// `keyFrame: true` option.
#[napi]
pub async fn force_keyframe() -> napi::Result<()> {
    #[cfg(target_os = "windows")]
    {
        let state_arc = state::shared();
        let guard = state_arc.lock().await;
        if let Some(engine) = &guard.video_engine {
            engine.request_keyframe();
        }
    }
    Ok(())
}

/// Enumerate DXGI adapters and return the first non-software adapter's
/// PCI vendor id. Used by `probe_native_encoders` to pick the right
/// encoder vendor priority (NVIDIA → NVENC first, etc.).
#[cfg(target_os = "windows")]
fn read_primary_gpu_vendor_id() -> u32 {
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1,
        DXGI_ADAPTER_FLAG_SOFTWARE,
    };
    unsafe {
        let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
            Ok(f) => f,
            Err(_) => return 0,
        };
        let mut i = 0u32;
        loop {
            let adapter: IDXGIAdapter1 = match factory.EnumAdapters1(i) {
                Ok(a) => a,
                Err(_) => return 0,
            };
            // windows-rs 0.61 returns the desc by value.
            let desc = match adapter.GetDesc1() {
                Ok(d) => d,
                Err(_) => {
                    i += 1;
                    continue;
                }
            };
            // Bit-mask check — DXGI_ADAPTER_FLAG_SOFTWARE is 2.
            // desc.Flags is u32 in windows-rs 0.61; FLAG_SOFTWARE
            // inner value is i32, so cast before AND.
            if (desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) == 0 {
                return desc.VendorId;
            }
            i += 1;
        }
    }
}
