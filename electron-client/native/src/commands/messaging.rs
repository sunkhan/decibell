//! Direct-message send command. The receive path goes via
//! net/central.rs::route_packets → events::emit_message_received with
//! `context: "dm"` — the renderer's useDmEvents hook listens for that
//! and routes it into useDmStore.

use crate::net::connection::build_packet;
use crate::net::proto::{packet, DirectMessage};
use crate::state;

#[napi(object)]
pub struct SendPrivateMessageArgs {
    pub recipient: String,
    pub message: String,
}

#[napi]
pub async fn send_private_message(args: SendPrivateMessageArgs) -> napi::Result<()> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let state_arc = state::shared();

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
                content: args.message,
                timestamp,
                // Server stamps the persisted id on the routed packet
                // after insertDm; outbound from client is always 0.
                id: 0,
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
