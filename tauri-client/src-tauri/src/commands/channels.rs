use tauri::State;

use crate::state::SharedState;

#[tauri::command]
pub async fn join_channel(
    server_id: String,
    channel_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    match s.communities.get_mut(&server_id) {
        Some(client) => client.join_channel(&channel_id).await,
        None => Err(format!("Not connected to community {}", server_id)),
    }
}

#[tauri::command]
pub async fn send_channel_message(
    server_id: String,
    channel_id: String,
    message: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let sender = s.username.clone().ok_or("Not authenticated")?;
    match s.communities.get(&server_id) {
        Some(client) => client.send_channel_message(&sender, &channel_id, &message).await,
        None => Err(format!("Not connected to community {}", server_id)),
    }
}
