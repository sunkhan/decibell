use tauri::State;

use crate::state::SharedState;

#[tauri::command]
pub async fn request_friend_list(
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let token = s.token.clone();
    match &s.central {
        Some(client) => client.request_friend_list(token.as_deref()).await,
        None => Err("Not connected to central server".to_string()),
    }
}

#[tauri::command]
pub async fn send_friend_action(
    action: i32,
    target_username: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let token = s.token.clone();
    match &s.central {
        Some(client) => {
            client
                .send_friend_action(action, &target_username, token.as_deref())
                .await
        }
        None => Err("Not connected to central server".to_string()),
    }
}
