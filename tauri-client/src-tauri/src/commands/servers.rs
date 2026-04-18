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
    connect_to_community_with_invite(server_id, host, port, None, app, state).await
}

/// Connect to a community server, optionally consuming an invite code so the
/// user joins (becomes a persistent member) as part of the handshake.
#[tauri::command]
pub async fn redeem_invite(
    server_id: String,
    host: String,
    port: u16,
    invite_code: String,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    if invite_code.trim().is_empty() {
        return Err("Invite code is empty".into());
    }
    connect_to_community_with_invite(
        server_id,
        host,
        port,
        Some(invite_code.trim().to_uppercase()),
        app,
        state,
    )
    .await
}

async fn connect_to_community_with_invite(
    server_id: String,
    host: String,
    port: u16,
    invite_code: Option<String>,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let state_arc = state.inner().clone();
    let jwt = {
        let s = state_arc.lock().await;
        s.token.clone().ok_or("Not authenticated")?
    };

    // If we already have a live session for this server, tear it down first —
    // a fresh redeem (or a re-connect with a new invite) should replace it.
    {
        let mut s = state_arc.lock().await;
        if let Some(mut existing) = s.communities.remove(&server_id) {
            existing.disconnect();
        }
    }

    let client = CommunityClient::connect(
        server_id.clone(),
        host,
        port,
        jwt,
        invite_code,
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
    let removed = {
        let mut s = state.lock().await;
        s.communities.remove(&server_id)
    };
    match removed {
        Some(mut client) => {
            client.disconnect();
            Ok(())
        }
        None => Err(format!("Not connected to community {}", server_id)),
    }
}
