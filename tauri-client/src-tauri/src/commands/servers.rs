use tauri::{AppHandle, State};

use crate::net::community::CommunityClient;
use crate::state::SharedState;

#[tauri::command]
pub async fn request_server_list(
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let token = s.token.clone();
    match &s.central {
        Some(client) => client.request_server_list(token.as_deref()).await,
        None => Err("Not connected to central server".to_string()),
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
