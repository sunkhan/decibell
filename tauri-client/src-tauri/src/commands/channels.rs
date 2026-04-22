use tauri::State;

use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state::SharedState;

#[tauri::command]
pub async fn send_channel_message(
    server_id: String,
    channel_id: String,
    message: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let (write_tx, data) = {
        let s = state.lock().await;
        let sender = s.username.clone().ok_or("Not authenticated")?;
        let client = s.communities.get(&server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        let tx = client.connection_write_tx()
            .ok_or("Community connection lost")?;
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        let pkt = build_packet(
            packet::Type::ChannelMsg,
            packet::Payload::ChannelMsg(ChannelMessage {
                sender: sender.into(),
                channel_id: channel_id.into(),
                content: message.into(),
                timestamp,
            }),
            Some(&client.jwt),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
}
