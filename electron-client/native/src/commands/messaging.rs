//! Direct-message send command. The receive path goes via
//! net/central.rs::route_packets → the E2EE DM worker →
//! events::emit_message_received with `context: "dm"` — the renderer's
//! useDmEvents hook listens for that and routes it into useDmStore.
//!
//! E2EE: when both sides have keys the body is sealed here (see
//! e2ee/session.rs::seal_outbound) and `content` carries only a fixed
//! placeholder for pre-E2EE clients; otherwise the text goes as before.

use crate::e2ee::session::{self as e2ee, Outbound, PLACEHOLDER};
use crate::net::connection::build_packet;
use crate::net::proto::{packet, DirectMessage};
use crate::state;

#[napi(object)]
pub struct SendPrivateMessageArgs {
    pub recipient: String,
    pub message: String,
    /// Id of the DM being replied to (0/absent = not a reply).
    pub reply_to: Option<i64>,
    /// Client nonce echoed back by central so the renderer can settle
    /// its optimistic bubble.
    pub nonce: Option<String>,
}

#[napi]
pub async fn send_private_message(args: SendPrivateMessageArgs) -> napi::Result<()> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let state_arc = state::shared();

    // Seal before taking the lock: resolving the peer's key can be a
    // round-trip through central.
    let (content, envelope) = match e2ee::seal_outbound(&state_arc, &args.recipient, &args.message)
        .await
        .map_err(napi::Error::from_reason)?
    {
        Outbound::Plain => (args.message, Vec::new()),
        Outbound::Sealed(wire) => (PLACEHOLDER.to_string(), wire),
    };

    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let token = s.token.clone();
        // Sender comes from app state — never trust the renderer to
        // claim an identity it shouldn't.
        let sender = s
            .username
            .clone()
            .ok_or_else(|| napi::Error::from_reason("Not signed in"))?;
        let central = s
            .central
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not connected to central server"))?;
        let tx = central
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Central connection lost"))?;
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let pkt = build_packet(
            packet::Type::DirectMsg,
            packet::Payload::DirectMsg(DirectMessage {
                sender,
                recipient: args.recipient,
                content,
                timestamp,
                // Server stamps the persisted id on the routed packet
                // after insertDm; outbound from client is always 0.
                id: 0,
                edited_at: 0,
                reply_to: args.reply_to.unwrap_or(0),
                // Server-resolved on broadcast; never set by the client.
                reply_to_sender: String::new(),
                reply_to_content: String::new(),
                nonce: args.nonce.unwrap_or_default(),
                envelope,
                reply_to_envelope: Vec::new(),
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
