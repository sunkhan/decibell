//! P2P DM call commands — the central-signaling half.
//!
//!   - `get_call_config()`: what the central we're logged into offers
//!     (`call_signaling` gate + STUN list) — read after `login_succeeded`.
//!   - `send_call_signal(args)`: ship one `CALL_SIGNAL` through central to
//!     a peer. Central stamps `from`, applies the DM policy on INVITE and
//!     answers PEER_OFFLINE / NOT_ALLOWED itself; the reply arrives on the
//!     `call_signal` event like any relayed signal.
//!
//! The media half (candidate gathering, hole punch, sealed VoiceEngine)
//! lives in `media::{stun, punch, media_socket, call_crypto}` and is
//! driven by `call_prepare` / `call_connect` / `call_end` below once the
//! transport milestone lands.

use crate::net::connection::build_packet;
use crate::net::proto::{
    call_candidate, call_signal, packet, CallCandidate, CallSignal, CallStreamMeta,
};
use crate::state;

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
