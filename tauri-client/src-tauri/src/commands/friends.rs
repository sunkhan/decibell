use tauri::State;

use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state::SharedState;

#[tauri::command]
pub async fn request_friend_list(
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
            packet::Type::FriendListReq,
            packet::Payload::FriendListReq(FriendListReq {}),
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
pub async fn send_friend_action(
    action: i32,
    target_username: String,
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
            packet::Type::FriendActionReq,
            packet::Payload::FriendActionReq(FriendActionReq {
                action,
                target_username: target_username.into(),
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
