use tauri::{AppHandle, State};

use crate::net::community::CommunityClient;
use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state::SharedState;

#[tauri::command]
pub async fn request_server_list(
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
            packet::Type::ServerListReq,
            packet::Payload::ServerListReq(ServerListRequest {}),
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
pub async fn connect_to_community(
    server_id: String,
    host: String,
    port: u16,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let state_arc = state.inner().clone();
    let jwt = {
        let s = state_arc.lock().await;
        s.token.clone().ok_or("Not authenticated")?
    };

    let client = CommunityClient::connect(
        server_id.clone(),
        host,
        port,
        jwt,
        app,
        state_arc.clone(),
    )
    .await?;

    let mut s = state_arc.lock().await;
    s.communities.insert(server_id, client);
    Ok(())
}

#[tauri::command]
pub async fn disconnect_from_community(
    server_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    match s.communities.remove(&server_id) {
        Some(mut client) => {
            client.disconnect();
            Ok(())
        }
        None => Err(format!("Not connected to community {}", server_id)),
    }
}
