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
use crate::media::{capture_audio_wasapi, AudioStreamEngine};

#[napi(object)]
pub struct StartScreenShareArgs {
    /// Community + channel to announce in. Both absent during a DM call —
    /// the renderer announces over central with CALL_SIGNAL STREAM_START
    /// instead, and the peer is the only receiver.
    pub server_id: Option<String>,
    pub channel_id: Option<String>,
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
    /// Linux-only opt-in: when true, native PipeWire/portal capture +
    /// FFmpeg (NVENC/VAAPI) encoding runs instead of the renderer's
    /// WebCodecs path (the renderer skips getDisplayMedia + VideoEncoder
    /// and does not pump `send_video_frame`). The renderer sets this when
    /// a hardware encoder was probed and the user hasn't opted out.
    /// Ignored on Windows (always native) and macOS (always renderer).
    pub native_encode: Option<bool>,
    /// Embed the mouse cursor in the captured video. Honoured by the
    /// native capture paths (XDG portal cursor_mode / wlr overlay_cursor /
    /// Windows WGC SetIsCursorCaptureEnabled). Defaults to true (show).
    pub include_cursor: Option<bool>,
    /// Share-audio application filter: mode (`selected` | `all_except` |
    /// `all`) and the ticked app identities. Both absent → the filter last
    /// stored on AppState (`set_stream_audio_filter`) applies; otherwise
    /// this pair replaces it. Unknown mode → `all`.
    pub audio_mode: Option<String>,
    pub audio_apps: Option<Vec<String>>,
}

#[napi]
pub async fn start_screen_share(args: StartScreenShareArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    // Linux-only: renderer opted into native encoding. Drives whether we
    // defer engine storage to attach the capture pipeline after the lock.
    let native_linux = cfg!(target_os = "linux") && args.native_encode.unwrap_or(false);
    // Windows native: resolve the hardware encoder list BEFORE taking the
    // AppState lock. The probe test-opens NVENC/AMF sessions (hundreds of
    // ms) — it ran under the lock on every Go Live before; now it's cached
    // process-wide and only probes (off the runtime) on a cache miss.
    #[cfg(target_os = "windows")]
    let native_caps = if args.native_encode.unwrap_or(true) {
        Some(
            tokio::task::spawn_blocking(cached_native_encoder_caps)
                .await
                .map_err(|e| napi::Error::from_reason(format!("encoder probe failed: {e}")))?,
        )
    } else {
        None
    };
    let (announce, deferred_engine, session_id) = {
        let mut s = state_arc.lock().await;
        if s.video_engine.is_some() {
            return Err(napi::Error::from_reason("Already sharing screen"));
        }
        // Resolve the share-audio app filter before anything else so the
        // capture starts with the right rule instead of correcting itself
        // a moment later. Stored back so the live popover and the next
        // stream see the same value.
        if args.audio_mode.is_some() || args.audio_apps.is_some() {
            s.stream_audio_filter = crate::media::stream_audio_filter::StreamAudioFilter::from_args(
                args.audio_mode.as_deref(),
                args.audio_apps.as_deref(),
            );
        }
        let in_call = s.active_call.is_some();
        let voice = s.voice_engine.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Must be in a voice channel or call to share screen")
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
        // Community channel: the MLS epoch keys seal every frame; a DM call
        // has no group (its socket is sealed instead).
        let mls_ring = s.voice_mls.as_ref().map(|h| h.ring.clone());

        // Community announcement (StartStreamReq). Skipped in a DM call:
        // the P2P peer learns about the stream via CALL_SIGNAL from the
        // renderer, and there is no community session to tell.
        let announce: Option<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)> = if in_call {
            None
        } else {
            let server_id = args.server_id.as_deref().ok_or_else(|| {
                napi::Error::from_reason("server_id required outside a call")
            })?;
            let channel_id = args.channel_id.clone().ok_or_else(|| {
                napi::Error::from_reason("channel_id required outside a call")
            })?;
            let client = s.communities.get(server_id).ok_or_else(|| {
                napi::Error::from_reason(format!("Not connected to community {}", server_id))
            })?;
            let tx = client.connection_write_tx().ok_or_else(|| {
                napi::Error::from_reason("Community connection lost")
            })?;
            let pkt = build_packet(
                packet::Type::StartStreamReq,
                packet::Payload::StartStreamReq(StartStreamRequest {
                    channel_id,
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
            Some((tx, pkt))
        };

        let mut engine = VideoEngine::start(media_socket, sender_id, self_username, mls_ring.clone());
        let session_id = engine.session_id();
        // Remember where the stream is announced so a later stop without
        // ids (main process, after a renderer reload/crash) can still tell
        // the community. A DM call has no community target.
        if !in_call {
            if let (Some(server_id), Some(channel_id)) = (&args.server_id, &args.channel_id) {
                engine.set_announce_target(Some((server_id.clone(), channel_id.clone())));
            }
        }

        // Windows: spin up the native capture + encoder pipeline. The
        // renderer never calls send_video_frame on Windows — capture
        // and encode happen here. On Linux/macOS the renderer keeps
        // owning capture (getDisplayMedia) and encode (WebCodecs) and
        // pumps frames via send_video_frame; nothing native-side to do
        // beyond constructing VideoEngine.
        //
        // native_encode=false is now honoured on Windows too: the
        // renderer's WebCodecs fallback (native start failed, or the
        // `decibell.win_native_encode` opt-out) drives capture + encode
        // itself and only needs the announcement + VideoEngine sender.
        // Stream audio currently only exists on the native path, so the
        // fallback streams without audio — degradation, not a blocker.
        // None defaults to true so older callers keep the native path.
        #[cfg(target_os = "windows")]
        if let Some(caps) = native_caps {
            let source_id = args.source_id.as_deref().ok_or_else(|| {
                napi::Error::from_reason("source_id required on Windows")
            })?;
            // Resolve the requested codec to a working FFmpeg encoder
            // name via the (cached) native probe. initial_codec=0 means
            // "auto" — pick the first HW encoder available (whatever
            // NVENC/AMF gave us). Otherwise honour the user's pick; error
            // if the probe didn't see that codec as available.
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
                    args.include_cursor.unwrap_or(true),
                    args.server_id.clone().unwrap_or_default(),
                    args.channel_id.clone().unwrap_or_default(),
                )
                .map_err(napi::Error::from_reason)?;

            // Stream audio: the process-loopback mixer captures whatever the
            // user's app filter allows (AppState::stream_audio_filter — set
            // from this call's audio_mode/audio_apps or the live popover),
            // always excluding our own process tree so participants don't
            // hear themselves echoed back. The picked window no longer
            // scopes the audio by itself; the picker flags its owner and
            // the user decides.
            //
            // Audio failures here are non-fatal: video keeps streaming,
            // the warning logs and the user gets a stream with no
            // audio. Better than the whole start failing because of an
            // audio device hiccup.
            if args.share_audio {
                let filter = s.stream_audio_filter.clone();
                match capture_audio_wasapi::ProcessLoopbackMixer::start(filter) {
                    Ok((frame_rx, mixer)) => {
                        log::info!("[stream-audio] process-loopback mixer started");
                        s.audio_stream_engine = Some(AudioStreamEngine::start(
                            frame_rx,
                            voice_socket,
                            audio_sender_id,
                            args.audio_bitrate_kbps,
                            Some(Box::new(mixer)),
                            mls_ring.clone(),
                        ));
                    }
                    Err(e) => {
                        log::warn!("[stream-audio] mixer start failed: {}", e);
                    }
                }
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            // source_id is ignored on the renderer (WebCodecs) path;
            // getDisplayMedia / ScreenCaptureKit handle the picker there.
            let _ = args.source_id;
        }

        // Linux native encode is attached AFTER the lock (below) because
        // the portal dialog blocks on the user — so defer storing the
        // engine. Every other path stores it here under the lock.
        if native_linux {
            (announce, Some(engine), session_id)
        } else {
            s.video_engine = Some(engine);
            (announce, None, session_id)
        }
    };

    // Linux native: bring up the portal + capture pipeline FIRST, then
    // announce. We must not hold the AppState lock across the portal
    // dialog (it blocks on the user for seconds), and we must not announce
    // before the pipeline is live — otherwise watchers (and the streamer's
    // own UI) see a "streaming" presence with no frames, and a portal
    // cancel would leave a dangling StartStreamReq. On start_linux failure
    // the owned `engine` drops here → Drop clears the frame sink.
    #[cfg(target_os = "linux")]
    if let Some(mut engine) = deferred_engine {
        let (target_codec, wire_byte) = resolve_linux_codec(args.initial_codec);
        engine
            .start_linux(
                args.source_id.as_deref().unwrap_or(""),
                target_codec,
                wire_byte,
                args.width,
                args.height,
                args.fps,
                args.video_bitrate_kbps,
                args.include_cursor.unwrap_or(true),
                args.server_id.clone().unwrap_or_default(),
                args.channel_id.clone().unwrap_or_default(),
            )
            .await
            .map_err(napi::Error::from_reason)?;

        // Stream audio (Linux): PipeWire tap of the applications the user's
        // filter allows, always minus Decibell's own output so watchers
        // don't hear themselves. The filter is the only audio rule for BOTH
        // window and monitor captures — the XDG portal hands us an opaque
        // video node with no window/PID/app identity, so the picked window
        // can't be mapped to its app here (Windows can). Non-fatal: a
        // capture failure logs and the video stream keeps running without
        // audio.
        let audio_engine = if args.share_audio {
            // Grab the voice socket + sender id + filter under a brief lock,
            // then release it before the (~100 ms) PipeWire capture +
            // null-sink setup so we don't stall other commands.
            let voice_ctx = {
                let s = state_arc.lock().await;
                let filter = s.stream_audio_filter.clone();
                let mls_ring = s.voice_mls.as_ref().map(|h| h.ring.clone());
                s.voice_engine
                    .as_ref()
                    .map(|v| (v.voice_socket(), v.sender_id().to_string(), filter, mls_ring))
            };
            match voice_ctx {
                Some((voice_socket, audio_sender_id, filter, mls_ring)) => {
                    match crate::media::capture_audio_pipewire::start_system_audio_capture(filter) {
                        Ok((frame_rx, tap)) => {
                            log::info!("[video-linux] sharing app audio (PipeWire tap minus self)");
                            Some(crate::media::AudioStreamEngine::start(
                                frame_rx,
                                voice_socket,
                                audio_sender_id,
                                args.audio_bitrate_kbps,
                                Some(Box::new(tap)),
                                mls_ring,
                            ))
                        }
                        Err(e) => {
                            log::warn!(
                                "[video-linux] system audio capture failed ({e}); video continues without audio"
                            );
                            None
                        }
                    }
                }
                None => None,
            }
        } else {
            None
        };

        if let Some((write_tx, data)) = announce {
            match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(napi::Error::from_reason("Connection closed")),
                Err(_) => return Err(napi::Error::from_reason("Send timed out")),
            }
        }
        let mut s = state_arc.lock().await;
        s.video_engine = Some(engine);
        if let Some(a) = audio_engine {
            s.audio_stream_engine = Some(a);
        }
        return Ok(());
    }
    #[cfg(not(target_os = "linux"))]
    let _ = deferred_engine;

    // Renderer-WebCodecs path (Linux without native, macOS) and Windows
    // native (engine already stored under the lock): announce now.
    let Some((write_tx, data)) = announce else {
        return Ok(());
    };
    let announce_err = match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(_)) => "Connection closed",
        Err(_) => "Send timed out",
    };
    // The announce failed after the engines were stored: the caller sees an
    // error (and may fall back to the renderer path), so take this
    // session's engines back out — otherwise the retry hits "Already
    // sharing screen" while a live native capture keeps running unseen.
    discard_session_engines(session_id).await;
    Err(napi::Error::from_reason(announce_err))
}

/// Remove the video engine (and the stream-audio engine started with it)
/// from AppState if it still belongs to `session_id`, and drop them off the
/// async runtime — dropping joins the native capture/encode threads.
async fn discard_session_engines(session_id: u64) {
    let (video, audio) = {
        let state_arc = state::shared();
        let mut s = state_arc.lock().await;
        if s.video_engine.as_ref().map(|v| v.session_id()) == Some(session_id) {
            (s.video_engine.take(), s.audio_stream_engine.take())
        } else {
            (None, None)
        }
    };
    if video.is_some() || audio.is_some() {
        // Detached: the error goes back to the renderer now, not after the
        // (bounded) thread joins.
        tokio::task::spawn_blocking(move || {
            drop(video);
            drop(audio);
        });
    }
}

/// Map the renderer's requested VideoCodec byte to a Linux encoder codec
/// + the wire byte stamped on packets. 0 ("auto") and 1 both resolve to
/// hardware H.264 (h264_nvenc/vaapi, the most broadly decodable). The
/// encoder itself falls back through its candidate list per codec.
#[cfg(target_os = "linux")]
fn resolve_linux_codec(initial: u8) -> (crate::media::caps::CodecKind, u8) {
    use crate::media::caps::CodecKind;
    let kind = match initial {
        2 => CodecKind::H264Sw,
        3 => CodecKind::H265,
        4 => CodecKind::Av1,
        _ => CodecKind::H264Hw,
    };
    (kind, kind as u8)
}

#[napi(object)]
pub struct StopScreenShareArgs {
    /// Absent during a DM call (no community to notify).
    pub server_id: Option<String>,
    pub channel_id: Option<String>,
}

#[napi]
pub async fn stop_screen_share(args: StopScreenShareArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    // Take the engines under the lock but DON'T drop them here: VideoEngine::
    // Drop → stop_linux()/stop_windows() joins the native encode/capture
    // threads, and joining under the AppState mutex stalls every other
    // command for the flush duration. Idempotent: StreamCapture.stop() and
    // external stop callers may both invoke this; the StopStreamReq is
    // best-effort so a dead community connection still tears down locally.
    let (video, audio, send) = {
        let mut s = state_arc.lock().await;
        let was_streaming = s.video_engine.is_some();
        let video = s.video_engine.take();
        let audio = s.audio_stream_engine.take();
        // Explicit ids win (unchanged behaviour). Without ids — the main
        // process stopping an orphaned share after a renderer reload/crash
        // — fall back to the channel the stream was announced in. A DM-call
        // stream has no target and no community to notify.
        let target: Option<(String, String)> = match args.server_id.clone() {
            Some(server_id) => Some((server_id, args.channel_id.clone().unwrap_or_default())),
            None => video
                .as_ref()
                .and_then(|v| v.announce_target())
                .map(|(sid, cid)| (sid.to_string(), cid.to_string())),
        };
        let send = if was_streaming && s.active_call.is_none() {
            target.and_then(|(server_id, channel_id)| {
                s.communities.get(&server_id).and_then(|client| {
                    client.connection_write_tx().map(|tx| {
                        let pkt = build_packet(
                            packet::Type::StopStreamReq,
                            packet::Payload::StopStreamReq(StopStreamRequest { channel_id }),
                            Some(&client.jwt),
                        );
                        (tx, pkt)
                    })
                })
            })
        } else {
            None
        };
        (video, audio, send)
    };

    // Drop (→ join native threads + clear_frame_sink) off the async runtime.
    // A wedged capture / encoder join would otherwise hang silently on a
    // blocking thread (and the next capture session may never get frames),
    // so time the teardown and shout if it overruns.
    if video.is_some() || audio.is_some() {
        let handle = tokio::task::spawn_blocking(move || {
            let t0 = std::time::Instant::now();
            drop(video);
            drop(audio);
            log::info!("[stream] engine teardown took {:?}", t0.elapsed());
        });
        tokio::spawn(async move {
            if tokio::time::timeout(std::time::Duration::from_secs(5), handle).await.is_err() {
                log::warn!("[stream] engine teardown still running after 5 s — capture/encoder thread wedged?");
            }
        });
    }

    if let Some((write_tx, data)) = send {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await;
    }
    Ok(())
}

#[napi(object)]
pub struct MoveStreamToChannelArgs {
    pub server_id: String,
    pub channel_id: String,
    pub fps: u32,
    pub width: u32,
    pub height: u32,
    pub video_bitrate_kbps: u32,
    pub share_audio: bool,
    pub initial_codec: u8,
    pub enforced_codec: u8,
}

/// Carry an already-running stream into a new voice channel WITHOUT restarting
/// capture/encode. The caller has already joined the new voice channel, so a
/// fresh VoiceEngine with new UDP sockets exists; here we re-point the video
/// (and stream-audio) senders at those sockets and re-announce the stream in
/// the new channel. Because capture keeps running there is no portal/WGC
/// re-prompt — the whole reason this exists instead of stop+start.
#[napi]
pub async fn move_stream_to_channel(args: MoveStreamToChannelArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let send = {
        let mut s = state_arc.lock().await;

        if s.video_engine.is_none() {
            return Err(napi::Error::from_reason("Not currently streaming"));
        }
        let voice = s
            .voice_engine
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not in a voice channel"))?;
        let media_socket = voice.media_socket();
        let voice_socket = voice.voice_socket();

        // Re-point the video sender at the new media socket and force a
        // keyframe so the new channel's watchers get an IDR immediately.
        if let Some(video) = s.video_engine.as_ref() {
            video.set_send_socket(media_socket);
            // Native-encode paths force their own keyframe; the renderer forces
            // one for the WebCodecs path.
            #[cfg(any(target_os = "linux", target_os = "windows"))]
            video.request_keyframe();
        }
        // Re-point stream audio too (present only when sharing audio).
        if let Some(audio) = s.audio_stream_engine.as_ref() {
            audio.set_socket(voice_socket);
        }
        // A later stop without ids must notify the NEW channel.
        if let Some(video) = s.video_engine.as_mut() {
            video.set_announce_target(Some((args.server_id.clone(), args.channel_id.clone())));
        }

        // Announce the stream in the new channel — the old channel's stream was
        // stopped server-side when we left it.
        s.communities.get(&args.server_id).and_then(|client| {
            client.connection_write_tx().map(|tx| {
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
                (tx, pkt)
            })
        })
    };

    match send {
        Some((write_tx, data)) => {
            match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await
            {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
                Err(_) => Err(napi::Error::from_reason("Send timed out")),
            }
        }
        None => Err(napi::Error::from_reason(format!(
            "Not connected to community {}",
            args.server_id
        ))),
    }
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
    use crate::media::video_packet::WIRE_DESCRIPTION_MAGIC;
    use crate::media::video_pipeline;

    // Runs on the Electron MAIN thread (sync napi fn behind ipcMain.handle),
    // once per encoded frame. It only copies the bytes (applying the
    // HEVC/AV1 description prefix while it's at it) and enqueues them;
    // sealing, packetising and the per-chunk sendto() happen on the sink's
    // `decibell-video-send` thread (see video_pipeline::submit_renderer_frame).
    //
    // No `state_arc.lock()` here — the dedicated frame-sink slot is read
    // under its own short-held mutex. Sync rather than async — no .await,
    // so napi-rs doesn't spawn a task per frame.
    //
    // A cleared sink is a benign race, not an error: stop_screen_share
    // clears the slot while the renderer's encoder is still draining its
    // last chunks. A full queue sheds the frame and asks the encoder for a
    // keyframe — also not an error for the caller.
    //
    // Every platform: Windows takes this path too when it falls back to the
    // renderer's WebCodecs encoder (native start failed, or the
    // `decibell.win_native_encode` opt-out); its native pipeline never
    // calls this.
    let data: &[u8] = args.data.as_ref();
    let description: Option<&[u8]> = args.description.as_ref().map(|d| d.as_ref());
    let _ = video_pipeline::submit_renderer_frame(args.codec, args.keyframe, || {
        // For HEVC/AV1 keyframes with a description, prepend the magic-tag
        // length-prefix so receivers strip it back out and surface the
        // description as a separate field. H.264 keyframes carry SPS/PPS
        // inline in Annex B and don't need this.
        match description {
            Some(desc) if args.keyframe && (args.codec == 3 || args.codec == 4) => {
                let mut wire = Vec::with_capacity(
                    WIRE_DESCRIPTION_MAGIC.len() + 4 + desc.len() + data.len(),
                );
                wire.extend_from_slice(&WIRE_DESCRIPTION_MAGIC);
                wire.extend_from_slice(&(desc.len() as u32).to_be_bytes());
                wire.extend_from_slice(desc);
                wire.extend_from_slice(data);
                wire
            }
            _ => data.to_vec(),
        }
    });
    Ok(())
}

#[napi(object)]
pub struct SetStreamFrameSinkArgs {
    /// Streamer whose encoded frames the renderer now (no longer) displays.
    pub username: String,
    pub enabled: bool,
}

/// Renderer-side stream-frame subscriptions, mirrored natively. The preload
/// bridge calls this when the first subscriber for a username appears
/// (enabled) and when the last one goes (disabled); frames for usernames
/// without a sink are dropped before the TSFN — and own-stream self-preview
/// isn't even copied. Sync, and sent over the same `decibell:invoke`
/// channel as the player's follow-up keyframe request, so the sink is in
/// place before that keyframe can arrive. NOT the watch filter
/// (media::WATCHED_STREAMS) — that one gates the server-side watch.
#[napi]
pub fn set_stream_frame_sink(args: SetStreamFrameSinkArgs) {
    crate::events::set_stream_frame_sink(&args.username, args.enabled);
}

/// Drop every stream-frame sink. The preload calls this when it loads (a
/// renderer reload leaves no subscribers behind).
#[napi]
pub fn clear_stream_frame_sinks() {
    crate::events::clear_stream_frame_sinks();
}

#[napi(object)]
pub struct WatchStreamArgs {
    pub server_id: String,
    pub channel_id: String,
    pub target_username: String,
}

#[napi]
pub async fn watch_stream(args: WatchStreamArgs) -> napi::Result<()> {
    // Record the watch so the video receive thread forwards this streamer's
    // frames (it drops frames for un-watched senders).
    crate::media::watch_stream_add(&args.target_username);
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
    crate::media::watch_stream_remove(&args.target_username);
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
    // Clone the write channel + build the packet under the lock, then
    // release it before awaiting the send.
    let parts = {
        let s = state_arc.lock().await;
        let client = s.communities.get(&args.server_id).ok_or_else(|| {
            napi::Error::from_reason(format!(
                "Not connected to community {}",
                args.server_id
            ))
        })?;
        client.thumbnail_send_parts(&args.channel_id, args.jpeg_data.as_ref())
    };
    let (write_tx, data) =
        parts.ok_or_else(|| napi::Error::from_reason("Community connection lost"))?;
    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
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
#[cfg(any(target_os = "windows", target_os = "linux"))]
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

/// Serialises native encoder probes: they test-open real hardware encoder
/// sessions, and two overlapping probes (boot + a settings re-probe) would
/// double the concurrent NVENC/AMF sessions for no benefit.
#[cfg(any(target_os = "windows", target_os = "linux"))]
static PROBE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Process-wide result of the last successful Windows encoder probe,
/// shared by `probe_native_encoders` (which refreshes it) and
/// `start_screen_share` (which reuses it). Each probe opens 3+ NVENC/AMF
/// sessions and loads/unloads the vendor runtime — once per process is
/// enough. An empty result is not cached, so a later start retries.
#[cfg(target_os = "windows")]
static NATIVE_CAPS_CACHE: std::sync::Mutex<Option<Vec<crate::media::encoder_probe::EncoderCap>>> =
    std::sync::Mutex::new(None);

/// Run the Windows encoder probe under PROBE_LOCK and cache the result.
/// Blocking — call from spawn_blocking.
#[cfg(target_os = "windows")]
fn probe_and_cache_native_encoders() -> Vec<crate::media::encoder_probe::EncoderCap> {
    let _g = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    probe_and_cache_locked()
}

/// Probe + cache; the caller holds PROBE_LOCK.
#[cfg(target_os = "windows")]
fn probe_and_cache_locked() -> Vec<crate::media::encoder_probe::EncoderCap> {
    let caps = crate::media::encoder_probe::run(read_primary_gpu_vendor_id());
    let mut cache = NATIVE_CAPS_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *cache = if caps.is_empty() { None } else { Some(caps.clone()) };
    caps
}

/// Cached encoder caps, probing (under PROBE_LOCK) only on a miss.
/// Blocking — call from spawn_blocking.
#[cfg(target_os = "windows")]
fn cached_native_encoder_caps() -> Vec<crate::media::encoder_probe::EncoderCap> {
    if let Some(caps) = NATIVE_CAPS_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return caps.clone();
    }
    // A concurrent probe may have filled the cache while we waited for
    // the probe lock — check again under it before probing ourselves.
    let _g = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(caps) = NATIVE_CAPS_CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return caps.clone();
    }
    probe_and_cache_locked()
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
    let caps = tokio::task::spawn_blocking(probe_and_cache_native_encoders)
    .await
    .map_err(|e| napi::Error::from_reason(format!("encoder probe failed: {}", e)))?;
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

/// Linux native FFmpeg encoder probe. Test-opens each codec's candidate
/// encoders (NVENC → VAAPI → software) and reports which work. Hardware
/// codecs advertise a 4K/60 ceiling; software libx264 is capped lower
/// since CPU 4K encoding isn't realtime.
///
/// Async + spawn_blocking for the same reason as the Windows variant: the
/// test-opens must not run on the Electron main thread.
#[cfg(target_os = "linux")]
#[napi]
pub async fn probe_native_encoders() -> napi::Result<Vec<NativeEncoderCap>> {
    let probed = tokio::task::spawn_blocking(|| {
        let _g = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::media::encoder_linux::probe_caps()
    })
    .await
    .map_err(|e| napi::Error::from_reason(format!("encoder probe failed: {}", e)))?;
    Ok(probed
        .into_iter()
        .map(|(kind, name, hardware)| {
            let (max_width, max_height, max_fps) = if hardware {
                (3840, 2160, 60)
            } else {
                (1920, 1080, 30)
            };
            NativeEncoderCap {
                codec: kind as i32,
                max_width,
                max_height,
                max_fps,
                hardware,
                encoder_name: name,
            }
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
    // Native pipelines (Windows always; Linux when native_encode is on)
    // own keyframe forcing. On macOS / the Linux renderer-fallback path
    // this is a no-op — WebCodecs forces keyframes JS-side.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    {
        let state_arc = state::shared();
        let guard = state_arc.lock().await;
        if let Some(engine) = &guard.video_engine {
            engine.request_keyframe();
        }
    }
    Ok(())
}

#[napi(object)]
pub struct RequestStreamKeyframeArgs {
    /// Username of the streamer whose next frame should be an IDR.
    pub username: String,
}

/// Watcher-side keyframe request: send a UdpKeyframeRequest (PLI) to
/// the named streamer over the media socket. The receive thread fires
/// these automatically on reassembly gaps, but the *renderer* also
/// drops frames (decoder-queue backpressure, decoder errors) that
/// native can't see — this command lets it re-request a keyframe
/// instead of freezing until the next natural IDR. Callers throttle.
/// Per-streamer throttle for renderer-driven PLIs. The renderer already gates
/// to 1/s, but don't trust it: a buggy or looping renderer must not be able to
/// flood a streamer (and the relay) with keyframe requests.
static KEYFRAME_REQ_LAST: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
> = std::sync::OnceLock::new();

#[napi]
pub async fn request_stream_keyframe(args: RequestStreamKeyframeArgs) -> napi::Result<()> {
    use crate::media::video_packet::UdpKeyframeRequest;
    {
        let map = KEYFRAME_REQ_LAST
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
        let mut guard = map.lock().unwrap();
        let now = std::time::Instant::now();
        if let Some(&last) = guard.get(&args.username) {
            if now.duration_since(last) < std::time::Duration::from_millis(500) {
                return Ok(()); // throttled — a PLI for this streamer went out recently
            }
        }
        guard.insert(args.username.clone(), now);
    }
    let (socket, sender_id) = {
        let state_arc = state::shared();
        let guard = state_arc.lock().await;
        match guard.voice_engine.as_ref() {
            Some(v) => (v.media_socket(), v.sender_id().to_string()),
            // Not in voice — nothing to request from; benign no-op.
            None => return Ok(()),
        }
    };
    let pli = UdpKeyframeRequest::new(&sender_id, &args.username);
    let _ = socket.send(&pli.to_bytes());
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
