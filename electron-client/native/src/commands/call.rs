//! P2P DM call commands.
//!
//! Central-signaling half:
//!   - `get_call_config()`: what the central we're logged into offers
//!     (`call_signaling` gate + STUN list) — read after `login_succeeded`.
//!   - `send_call_signal(args)`: ship one `CALL_SIGNAL` through central to
//!     a peer. Central stamps `from`, applies the DM policy on INVITE and
//!     answers PEER_OFFLINE / NOT_ALLOWED itself; the reply arrives on the
//!     `call_signal` event like any relayed signal.
//!
//! Media half (see `media::{media_socket, call_crypto, stun, punch}`):
//!   - `call_prepare({callId, peer})`: bind the voice + media sockets, learn
//!     their reflexive addresses via STUN, enumerate host addresses, mint an
//!     ephemeral X25519 keypair → `{pubKey, candidates}` for INVITE/ACCEPT.
//!   - `call_connect({… remotePubKey, remoteCandidates})`: derive the
//!     per-call keys, seal both sockets, hole-punch them in parallel, then
//!     start the VoiceEngine in P2P mode. Returns immediately; the outcome
//!     is `call_connected` / `call_failed`, and a 15 s peer-silence
//!     watchdog later emits `call_dropped`.
//!   - `call_end()`: abort a punch in flight or tear the call's engines
//!     down. Idempotent.
//!   - `call_watch_stream({watch})`: gate the peer's stream frames through
//!     to the renderer (the P2P analogue of watch_stream / stop_watching).
//!
//! Mutual exclusion with community voice: the single `AppState.voice_engine`
//! belongs to whichever started first. `call_prepare` / `call_connect`
//! refuse while a channel session is up; `join_voice_channel` refuses
//! while `active_call` is set. The renderer leaves / hangs up first.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::events;
use crate::media::call_crypto;
use crate::media::media_socket::MediaSocket;
use crate::media::punch::{self, CandidateKind, RemoteCandidate};
use crate::media::stun;
use crate::media::{configure_udp_socket, watch_stream_add, watch_stream_remove, VoiceEngine};
use crate::net::connection::build_packet;
use crate::net::proto::{
    call_candidate, call_signal, packet, CallCandidate, CallSignal, CallStreamMeta,
};
use crate::state::{self, ActiveCall, PendingCall};

/// Whole hole-punch budget per call (both sockets run in parallel).
const PUNCH_DEADLINE: Duration = Duration::from_secs(10);
/// No authenticated datagram on the voice socket for this long → the peer
/// is gone (their keepalives run every 500 ms, PINGs every 3 s).
const PEER_LOST_AFTER: Duration = Duration::from_secs(15);
const MAX_HOST_IPS: usize = 6;

// ── Central signaling ────────────────────────────────────────────────

#[napi(object)]
pub struct CallConfig {
    pub call_signaling: bool,
    pub stun_servers: Vec<String>,
}

#[napi]
pub async fn get_call_config() -> napi::Result<CallConfig> {
    let state_arc = state::shared();
    let s = state_arc.lock().await;
    Ok(CallConfig {
        call_signaling: s.call_signaling,
        stun_servers: s.stun_servers.clone(),
    })
}

#[napi(object)]
#[derive(Clone)]
pub struct CallCandidateArg {
    /// "VOICE" | "MEDIA"
    pub socket: String,
    /// "HOST" | "SRFLX"
    pub kind: String,
    pub ip: String,
    pub port: u32,
}

#[napi(object)]
pub struct CallStreamMetaArg {
    pub codec: i32,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub has_audio: bool,
}

#[napi(object)]
pub struct SendCallSignalArgs {
    pub call_id: String,
    pub to: String,
    /// proto CallSignal.Kind name: "INVITE", "ACCEPT", "HANGUP", …
    pub kind: String,
    /// base64 X25519 public key — INVITE / ACCEPT only.
    pub pub_key: Option<String>,
    pub candidates: Option<Vec<CallCandidateArg>>,
    pub stream: Option<CallStreamMetaArg>,
}

#[napi]
pub async fn send_call_signal(args: SendCallSignalArgs) -> napi::Result<()> {
    use base64::Engine as _;

    let kind = call_signal::Kind::from_str_name(&args.kind)
        .ok_or_else(|| napi::Error::from_reason(format!("Unknown call signal kind {}", args.kind)))?;
    let pub_key = match args.pub_key.as_deref() {
        Some(b64) if !b64.is_empty() => base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| napi::Error::from_reason(format!("Bad pub_key base64: {e}")))?,
        _ => Vec::new(),
    };
    let mut candidates = Vec::new();
    for c in args.candidates.unwrap_or_default() {
        let socket = call_candidate::Socket::from_str_name(&c.socket)
            .ok_or_else(|| napi::Error::from_reason(format!("Unknown candidate socket {}", c.socket)))?;
        let ckind = call_candidate::Kind::from_str_name(&c.kind)
            .ok_or_else(|| napi::Error::from_reason(format!("Unknown candidate kind {}", c.kind)))?;
        candidates.push(CallCandidate {
            socket: socket as i32,
            kind: ckind as i32,
            ip: c.ip,
            port: c.port,
        });
    }
    let stream = args.stream.map(|m| CallStreamMeta {
        codec: m.codec,
        width: m.width,
        height: m.height,
        fps: m.fps,
        has_audio: m.has_audio,
    });

    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let central = s
            .central
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not connected to central server"))?;
        let tx = central
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Central connection lost"))?;
        let token = s.token.clone();
        let pkt = build_packet(
            packet::Type::CallSignal,
            packet::Payload::CallSignal(CallSignal {
                kind: kind as i32,
                call_id: args.call_id,
                // Central overwrites `from` with the authenticated username;
                // sending it empty makes that explicit.
                from: String::new(),
                to: args.to,
                pub_key,
                candidates,
                stream,
                timestamp: 0,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

// ── Candidate gathering ──────────────────────────────────────────────

/// Local IPv4 addresses worth advertising as host candidates: every
/// non-loopback, non-link-local interface (LAN, VPN, bridges), capped.
/// Falls back to the default-route source address if enumeration yields
/// nothing.
fn host_ips() -> Vec<std::net::Ipv4Addr> {
    let mut out: Vec<std::net::Ipv4Addr> = Vec::new();
    match if_addrs::get_if_addrs() {
        Ok(ifaces) => {
            for i in ifaces {
                if let IpAddr::V4(v4) = i.ip() {
                    if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() {
                        continue;
                    }
                    if !out.contains(&v4) {
                        out.push(v4);
                    }
                }
            }
        }
        Err(e) => log::warn!("[call] interface enumeration failed: {e}"),
    }
    if out.is_empty() {
        // `connect` on a UDP socket picks the outbound interface without
        // sending anything.
        if let Ok(s) = UdpSocket::bind("0.0.0.0:0") {
            if s.connect("1.1.1.1:53").is_ok() {
                if let Ok(SocketAddr::V4(a)) = s.local_addr() {
                    out.push(*a.ip());
                }
            }
        }
    }
    out.truncate(MAX_HOST_IPS);
    out
}

fn random_txid() -> [u8; 12] {
    use ring::rand::SecureRandom;
    let mut t = [0u8; 12];
    let _ = ring::rand::SystemRandom::new().fill(&mut t);
    t
}

fn candidate(socket: &str, kind: &str, ip: impl ToString, port: u16) -> CallCandidateArg {
    CallCandidateArg {
        socket: socket.to_string(),
        kind: kind.to_string(),
        ip: ip.to_string(),
        port: port as u32,
    }
}

fn parse_remote(cands: &[CallCandidateArg], socket: &str) -> Vec<RemoteCandidate> {
    let mut out: Vec<RemoteCandidate> = Vec::new();
    for c in cands.iter().filter(|c| c.socket == socket) {
        let Ok(ip) = c.ip.parse::<IpAddr>() else { continue };
        if !ip.is_ipv4() {
            continue;
        }
        let Ok(port) = u16::try_from(c.port) else { continue };
        if port == 0 {
            continue;
        }
        let addr = SocketAddr::new(ip, port);
        if out.iter().any(|r| r.addr == addr) {
            continue;
        }
        let kind = if c.kind == "HOST" { CandidateKind::Host } else { CandidateKind::Srflx };
        out.push(RemoteCandidate { addr, kind });
    }
    out
}

// ── call_prepare ─────────────────────────────────────────────────────

#[napi(object)]
pub struct CallPrepareArgs {
    pub call_id: String,
    pub peer: String,
}

#[napi(object)]
pub struct CallPrepareResult {
    /// base64 of our 32-byte X25519 public key.
    pub pub_key: String,
    pub candidates: Vec<CallCandidateArg>,
}

#[napi]
pub async fn call_prepare(args: CallPrepareArgs) -> napi::Result<CallPrepareResult> {
    use base64::Engine as _;

    let state_arc = state::shared();
    let (stun_servers, username) = {
        let mut s = state_arc.lock().await;
        if s.active_call.is_some() {
            return Err(napi::Error::from_reason("Already in a call"));
        }
        if s.voice_engine.is_some() {
            return Err(napi::Error::from_reason("Leave the voice channel first"));
        }
        // A newer prepare supersedes an older one — its sockets just close.
        s.pending_call = None;
        let username = s
            .username
            .clone()
            .ok_or_else(|| napi::Error::from_reason("Not authenticated"))?;
        let servers: Vec<String> = if s.stun_servers.is_empty() {
            stun::DEFAULT_STUN_SERVERS.iter().map(|x| x.to_string()).collect()
        } else {
            s.stun_servers.clone()
        };
        (servers, username)
    };
    if args.peer == username {
        return Err(napi::Error::from_reason("Cannot call yourself"));
    }

    // Bind + STUN off the async runtime: each query blocks up to 2.5 s, the
    // two sockets are queried in parallel.
    let gathered = tokio::task::spawn_blocking(
        move || -> Result<(UdpSocket, UdpSocket, Vec<CallCandidateArg>), String> {
            let voice = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("voice bind: {e}"))?;
            let media = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("media bind: {e}"))?;
            configure_udp_socket(&voice);
            configure_udp_socket(&media);
            let vport = voice.local_addr().map_err(|e| e.to_string())?.port();
            let mport = media.local_addr().map_err(|e| e.to_string())?.port();

            let voice_c = voice.try_clone().map_err(|e| e.to_string())?;
            let media_c = media.try_clone().map_err(|e| e.to_string())?;
            let servers_v = stun_servers.clone();
            let servers_m = stun_servers;
            let tv = std::thread::spawn(move || stun::query(&voice_c, &servers_v, &random_txid()));
            let tm = std::thread::spawn(move || stun::query(&media_c, &servers_m, &random_txid()));
            let srflx_v = tv.join().ok().flatten();
            let srflx_m = tm.join().ok().flatten();

            let mut cands = Vec::new();
            for ip in host_ips() {
                cands.push(candidate("VOICE", "HOST", ip, vport));
                cands.push(candidate("MEDIA", "HOST", ip, mport));
            }
            match srflx_v {
                Some(a) => cands.push(candidate("VOICE", "SRFLX", a.ip(), a.port())),
                None => log::warn!("[call] no STUN answer for the voice socket — host candidates only"),
            }
            match srflx_m {
                Some(a) => cands.push(candidate("MEDIA", "SRFLX", a.ip(), a.port())),
                None => log::warn!("[call] no STUN answer for the media socket — host candidates only"),
            }
            if cands.is_empty() {
                return Err("No usable network address found".to_string());
            }
            Ok((voice, media, cands))
        },
    )
    .await
    .map_err(|e| napi::Error::from_reason(format!("candidate gathering failed: {e}")))?
    .map_err(napi::Error::from_reason)?;

    let (voice, media, candidates) = gathered;
    let local = call_crypto::generate().map_err(napi::Error::from_reason)?;
    let pub_key = base64::engine::general_purpose::STANDARD.encode(local.public);
    log::info!(
        "[call] prepared {} → {}: {} candidates",
        args.call_id,
        args.peer,
        candidates.len()
    );

    {
        let mut s = state_arc.lock().await;
        if s.active_call.is_some() {
            return Err(napi::Error::from_reason("Already in a call"));
        }
        s.pending_call = Some(PendingCall {
            call_id: args.call_id,
            peer: args.peer,
            local,
            voice,
            media,
        });
    }
    Ok(CallPrepareResult { pub_key, candidates })
}

// ── call_connect ─────────────────────────────────────────────────────

#[napi(object)]
pub struct CallConnectArgs {
    pub call_id: String,
    pub peer: String,
    /// base64 of the peer's 32-byte X25519 public key.
    pub remote_pub_key: String,
    pub remote_candidates: Vec<CallCandidateArg>,
    pub voice_bitrate_kbps: Option<i32>,
}

#[napi]
pub async fn call_connect(args: CallConnectArgs) -> napi::Result<()> {
    use base64::Engine as _;

    let peer_pub = base64::engine::general_purpose::STANDARD
        .decode(&args.remote_pub_key)
        .map_err(|e| napi::Error::from_reason(format!("Bad remote pub_key: {e}")))?;
    let bitrate_bps = match args.voice_bitrate_kbps {
        Some(k) if k > 0 => k * 1000,
        _ => crate::media::codec::OpusEncoder::DEFAULT_BITRATE_BPS,
    };

    let state_arc = state::shared();
    let (pending, username, stop) = {
        let mut s = state_arc.lock().await;
        if s.active_call.is_some() {
            return Err(napi::Error::from_reason("Already in a call"));
        }
        if s.voice_engine.is_some() {
            return Err(napi::Error::from_reason("Leave the voice channel first"));
        }
        let pending = s
            .pending_call
            .take()
            .ok_or_else(|| napi::Error::from_reason("Call not prepared"))?;
        if pending.call_id != args.call_id || pending.peer != args.peer {
            return Err(napi::Error::from_reason("Prepared call does not match"));
        }
        let username = s
            .username
            .clone()
            .ok_or_else(|| napi::Error::from_reason("Not authenticated"))?;
        let stop = Arc::new(AtomicBool::new(false));
        s.active_call = Some(ActiveCall {
            call_id: args.call_id.clone(),
            peer: args.peer.clone(),
            stop: stop.clone(),
            watchdog: None,
        });
        (pending, username, stop)
    };

    let keys = match call_crypto::derive(pending.local, &peer_pub, &args.call_id, &username, &args.peer) {
        Ok(k) => k,
        Err(e) => {
            let mut s = state_arc.lock().await;
            s.active_call = None;
            return Err(napi::Error::from_reason(format!("Key agreement failed: {e}")));
        }
    };
    let voice = Arc::new(MediaSocket::sealed(pending.voice, keys.voice, &username));
    let media = Arc::new(MediaSocket::sealed(pending.media, keys.media, &username));
    let remote_voice = parse_remote(&args.remote_candidates, "VOICE");
    let remote_media = parse_remote(&args.remote_candidates, "MEDIA");

    // Saved device prefs, same as join_voice_channel.
    let (saved_input, saved_output) = match crate::config::load() {
        Ok(loaded) => (
            loaded.settings.input_device.clone(),
            loaded.settings.output_device.clone(),
        ),
        Err(_) => (None, None),
    };

    let call_id = args.call_id;
    let peer = args.peer;
    tokio::spawn(async move {
        let pv = {
            let v = voice.clone();
            let stop = stop.clone();
            let u = username.clone();
            tokio::task::spawn_blocking(move || punch::punch(&v, &u, &remote_voice, PUNCH_DEADLINE, &stop))
        };
        let pm = {
            let m = media.clone();
            let stop = stop.clone();
            let u = username.clone();
            tokio::task::spawn_blocking(move || punch::punch(&m, &u, &remote_media, PUNCH_DEADLINE, &stop))
        };
        let (rv, rm) = tokio::join!(pv, pm);
        let outcome = match (rv, rm) {
            (Ok(Ok(v)), Ok(Ok(m))) => Ok((v, m)),
            (Ok(Err(e)), _) | (_, Ok(Err(e))) => Err(e),
            _ => Err(punch::PunchError::Io("punch task panicked".to_string())),
        };

        let (pv, pm) = match outcome {
            Ok(x) => x,
            Err(e) => {
                let (reason, detail) = match e {
                    punch::PunchError::NoPath => (
                        "no_path",
                        "No direct path between the two networks (symmetric NAT or firewall)".to_string(),
                    ),
                    punch::PunchError::Aborted => ("aborted", String::new()),
                    punch::PunchError::Io(m) => ("bind", m),
                };
                log::warn!("[call] {} punch failed: {} {}", call_id, reason, detail);
                {
                    let mut s = state_arc.lock().await;
                    if s.active_call.as_ref().map(|a| a.call_id == call_id).unwrap_or(false) {
                        s.active_call = None;
                    }
                }
                if reason != "aborted" {
                    events::emit_call_failed(events::CallFailedPayload {
                        call_id,
                        reason: reason.to_string(),
                        detail,
                    });
                }
                return;
            }
        };
        log::info!(
            "[call] {} punched: voice {} ({}), media {} ({})",
            call_id,
            pv.peer,
            pv.kind.as_str(),
            pm.peer,
            pm.kind.as_str()
        );

        let mut engine = match VoiceEngine::start_p2p(
            voice.clone(),
            media,
            username.clone(),
            bitrate_bps,
            saved_input,
            saved_output,
        ) {
            Ok(e) => e,
            Err(e) => {
                let mut s = state_arc.lock().await;
                if s.active_call.as_ref().map(|a| a.call_id == call_id).unwrap_or(false) {
                    s.active_call = None;
                }
                drop(s);
                events::emit_call_failed(events::CallFailedPayload {
                    call_id,
                    reason: "bind".to_string(),
                    detail: e,
                });
                return;
            }
        };

        let (is_muted, is_deafened) = {
            let mut s = state_arc.lock().await;
            let still_ours = s.active_call.as_ref().map(|a| a.call_id == call_id).unwrap_or(false);
            if stop.load(Ordering::Relaxed) || !still_ours || s.voice_engine.is_some() {
                // call_end raced us (or a channel join slipped in): tear the
                // fresh engine straight back down.
                drop(s);
                crate::commands::voice::stop_voice_engine_background(Some(engine));
                return;
            }
            // Restore persisted mute/deafen like join_voice_channel does.
            let saved_muted = s.voice_muted;
            let saved_deafened = s.voice_deafened;
            let saved_mbd = s.voice_muted_before_deafen;
            if saved_deafened {
                if saved_mbd {
                    engine.set_mute(true);
                }
                engine.set_deafen(true);
            } else if saved_muted {
                engine.set_mute(true);
            }
            engine.set_muted_before_deafen(saved_mbd);
            let is_muted = engine.is_muted();
            let is_deafened = engine.is_deafened();
            s.voice_engine = Some(engine);

            let wd_stop = stop.clone();
            let wd_voice = voice.clone();
            let wd_call = call_id.clone();
            let watchdog = tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    if wd_stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if wd_voice.last_rx_age() > PEER_LOST_AFTER {
                        log::warn!("[call] {} peer silent for {:?} — dropping", wd_call, PEER_LOST_AFTER);
                        events::emit_call_dropped(events::CallDroppedPayload {
                            call_id: wd_call,
                            reason: "peer_lost".to_string(),
                        });
                        break;
                    }
                }
            });
            if let Some(ac) = s.active_call.as_mut() {
                ac.watchdog = Some(watchdog);
            }
            (is_muted, is_deafened)
        };

        events::emit_voice_state_changed(is_muted, is_deafened);
        events::emit_call_connected(events::CallConnectedPayload {
            call_id,
            rtt_ms: pv.rtt_ms.or(pm.rtt_ms).unwrap_or(0.0),
            path: pv.kind.as_str().to_string(),
        });
        let _ = peer;
    });
    Ok(())
}

// ── call_end ─────────────────────────────────────────────────────────

#[napi]
pub async fn call_end() -> napi::Result<()> {
    let state_arc = state::shared();
    let (old_voice, video, audio, is_muted, is_deafened, had_call) = {
        let mut s = state_arc.lock().await;
        s.pending_call = None;
        let had_call = s.active_call.is_some();
        if let Some(ac) = s.active_call.take() {
            ac.stop.store(true, Ordering::Relaxed);
            if let Some(h) = ac.watchdog {
                h.abort();
            }
        }
        // The engine is the call's only when no channel session owns it.
        let (old_voice, video, audio) = if had_call && s.connected_voice_channel.is_none() {
            let (m, d, mbd) = s
                .voice_engine
                .as_ref()
                .map(|e| (e.is_muted(), e.is_deafened(), e.muted_before_deafen()))
                .unwrap_or((s.voice_muted, s.voice_deafened, s.voice_muted_before_deafen));
            s.voice_muted = m;
            s.voice_deafened = d;
            s.voice_muted_before_deafen = mbd;
            // Synchronously — a watch registered for the NEXT call must not
            // be wiped by this engine's deferred stop.
            crate::media::watched_streams_clear();
            (
                s.voice_engine.take(),
                s.video_engine.take(),
                s.audio_stream_engine.take(),
            )
        } else {
            (None, None, None)
        };
        (old_voice, video, audio, s.voice_muted, s.voice_deafened, had_call)
    };

    // Engine drops join native threads — keep that off the async runtime,
    // and shout if a join wedges (see stop_screen_share).
    if video.is_some() || audio.is_some() {
        let handle = tokio::task::spawn_blocking(move || {
            let t0 = std::time::Instant::now();
            drop(video);
            drop(audio);
            log::info!("[call] stream engine teardown took {:?}", t0.elapsed());
        });
        tokio::spawn(async move {
            if tokio::time::timeout(std::time::Duration::from_secs(5), handle).await.is_err() {
                log::warn!("[call] stream engine teardown still running after 5 s — capture/encoder thread wedged?");
            }
        });
    }
    crate::commands::voice::stop_voice_engine_background(old_voice);
    if had_call {
        events::emit_voice_state_changed(is_muted, is_deafened);
    }
    Ok(())
}

// ── call_watch_stream ────────────────────────────────────────────────

#[napi(object)]
pub struct CallWatchStreamArgs {
    pub watch: bool,
}

#[napi]
pub async fn call_watch_stream(args: CallWatchStreamArgs) -> napi::Result<()> {
    let peer = {
        let state_arc = state::shared();
        let s = state_arc.lock().await;
        s.active_call
            .as_ref()
            .map(|a| a.peer.clone())
            .ok_or_else(|| napi::Error::from_reason("Not in a call"))?
    };
    if args.watch {
        watch_stream_add(&peer);
    } else {
        watch_stream_remove(&peer);
    }
    Ok(())
}
