use tauri::State;

use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state::SharedState;

#[tauri::command]
pub async fn send_private_message(
    recipient: String,
    message: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let (write_tx, data) = {
        let s = state.lock().await;
        let sender = s.username.clone().ok_or("Not authenticated")?;
        let token = s.token.clone();
        let tx = s.central.as_ref()
            .ok_or("Not connected to central server")?
            .connection_write_tx()
            .ok_or("Central connection lost")?;
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let pkt = build_packet(
            packet::Type::DirectMsg,
            packet::Payload::DirectMsg(DirectMessage {
                sender: sender.into(),
                recipient: recipient.into(),
                content: message.into(),
                timestamp,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
}

#[tauri::command]
pub async fn set_dm_privacy(
    friends_only: bool,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let (write_tx, data) = {
        let s = state.lock().await;
        let token = s.token.clone();
        let tx = s.central.as_ref()
            .ok_or("Not connected to central server")?
            .connection_write_tx()
            .ok_or("Central connection lost")?;
        let pkt = build_packet(
            packet::Type::DmPrivacy,
            packet::Payload::DmPrivacy(DmPrivacySetting {
                friends_only,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
}
